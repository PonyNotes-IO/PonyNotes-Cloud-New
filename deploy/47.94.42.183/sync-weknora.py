"""Mirror authorized cloud notes into one WeKnora knowledge base per workspace."""
import argparse
import base64
from collections import Counter, deque
import hashlib
import hmac
import json
import os
from pathlib import Path
import re
import secrets
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

ROOT = Path('/opt/ponynotes-weknora')
CLOUD_ROOT = Path('/opt/ponynotes')
CHANNEL = re.compile(r'^pn:[0-9a-f]{40}$')


class APIError(RuntimeError):
    def __init__(self, status, code=None):
        self.status = status
        super().__init__(f'API status={status}, code={code}')


def request(base, path, method='GET', payload=None, headers=None):
    req = urllib.request.Request(base + path, method=method,
        data=json.dumps(payload, ensure_ascii=False).encode() if payload is not None else None,
        headers={'Content-Type': 'application/json', **(headers or {})})
    try:
        with urllib.request.urlopen(req, timeout=90) as response:
            result = json.load(response)
    except urllib.error.HTTPError as error:
        # Response bodies can contain private notes or credentials.
        raise APIError(error.code) from None
    if isinstance(result, dict) and (result.get('success') is False or result.get('code', 0) != 0):
        raise APIError(200, result.get('code'))
    return result


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, ensure_ascii=False).encode()).hexdigest()


def save(path, data):
    temporary = path.with_suffix('.tmp')
    with open(temporary, 'w', encoding='utf-8', opener=lambda p, f: os.open(p, f, 0o600)) as stream:
        json.dump(data, stream, ensure_ascii=False, indent=2)
    os.replace(temporary, path)
    path.chmod(0o600)


def sql(query):
    result = subprocess.run(['docker', 'compose', 'exec', '-T', 'postgres', 'psql',
        '-U', 'postgres', '-d', 'ponynotes', '-At', '-v', 'ON_ERROR_STOP=1'],
        cwd=CLOUD_ROOT, input=query, text=True, capture_output=True, check=True)
    return json.loads(result.stdout)


def cloud_users():
    return sql("SELECT coalesce(json_agg(t),'[]') FROM (SELECT u.uuid,u.email,u.name "
        "FROM af_user u JOIN auth.users a ON a.id=u.uuid WHERE a.deleted_at IS NULL "
        "AND (a.banned_until IS NULL OR a.banned_until<now())) t;")


def cloud_workspaces():
    # Keep workspaces with no active members in this inventory: account removal
    # must never be mistaken for deletion of a shared knowledge base.
    return sql("SELECT coalesce(json_agg(t),'[]') FROM (SELECT w.workspace_id, "
        "w.workspace_name, owner.uuid AS owner_uuid, "
        "coalesce((SELECT json_agg(u.uuid ORDER BY u.uuid) FROM af_workspace_member m "
        "JOIN af_user u ON u.uid=m.uid JOIN auth.users a ON a.id=u.uuid "
        "WHERE m.workspace_id=w.workspace_id AND m.role_id<>3 AND a.deleted_at IS NULL "
        "AND (a.banned_until IS NULL OR a.banned_until<now())), '[]') AS members "
        "FROM af_workspace w LEFT JOIN af_user owner ON owner.uid=w.owner_uid "
        "WHERE w.deleted_at IS NULL AND coalesce(w.is_initialized,true)) t;")


def cloud_secret():
    env = json.loads(subprocess.check_output(['docker', 'inspect', 'ponynotes-gotrue-1']))[0]['Config']['Env']
    return next(x.split('=', 1)[1] for x in env if x.startswith('GOTRUE_JWT_SECRET='))


def billing_authorization(wid):
    wid = str(uuid.UUID(wid))
    owners = sql("SELECT coalesce(json_agg(t),'[]') FROM (SELECT u.uuid,u.email FROM af_workspace w "
        "JOIN af_user u ON u.uid=w.owner_uid JOIN auth.users a ON a.id=u.uuid "
        f"WHERE w.workspace_id='{wid}'::uuid AND w.deleted_at IS NULL "
        "AND coalesce(w.is_initialized,true) AND a.deleted_at IS NULL "
        "AND (a.banned_until IS NULL OR a.banned_until<now())) t;")
    if len(owners) != 1:
        raise APIError(403)
    # Capture the live owner before inference. Ownership transfer never rewrites past usage.
    return 'Bearer ' + jwt(owners[0], cloud_secret())


def record_tokens(authorization, tokens):
    if type(tokens) is not int or not 0 <= tokens <= 2**63 - 1:
        raise ValueError('Provider token usage missing or invalid')
    if tokens:
        request('http://127.0.0.1', '/api/subscription/usage/record', 'POST',
            {'usage_type': 'ai_tokens', 'usage_count': tokens}, {'Authorization': authorization})


def jwt(user, secret):
    encode = lambda value: base64.urlsafe_b64encode(value).rstrip(b'=')
    now = int(time.time())
    claims = {'sub': user['uuid'], 'aud': 'authenticated', 'exp': now + 600, 'iat': now,
        'email': user.get('email') or '', 'phone': '', 'role': 'authenticated',
        'app_metadata': {}, 'user_metadata': {}}
    body = b'.'.join([encode(b'{"alg":"HS256","typ":"JWT"}'), encode(json.dumps(claims).encode())])
    return (body + b'.' + encode(hmac.new(secret.encode(), body, hashlib.sha256).digest())).decode()


def document_markdown(document):
    """Walk only live, reachable blocks, in the document's child order."""
    blocks, meta = document['blocks'], document['meta']
    output, visited = [], set()
    stack = [(document['page_id'], 0)]
    while stack:
        block_id, depth = stack.pop()
        if block_id in visited:
            raise ValueError('Repeated/cyclic document block')
        visited.add(block_id)
        block = blocks[block_id]
        data = block.get('data') or {}
        if isinstance(data, str):
            data = json.loads(data)
        external = block.get('external_id')
        text = meta['text_map'][external] if external else ''
        if not isinstance(text, str):
            raise ValueError('Unexpected document text format')
        kind = block['ty']
        if kind == 'heading':
            text = '#' * max(1, min(6, int(data.get('level', 1)))) + ' ' + text
        elif kind in ('bulleted_list', 'numbered_list', 'todo_list'):
            prefix = {'bulleted_list': '- ', 'numbered_list': '1. ',
                'todo_list': '- [x] ' if data.get('checked') else '- [ ] '}[kind]
            text = '  ' * max(0, depth - 1) + prefix + text
        elif kind in ('quote', 'callout'):
            text = '> ' + text.replace('\n', '\n> ')
        elif kind == 'code':
            fence = '`' * max(3, max((len(x) + 1 for x in re.findall(r'`+', text)), default=3))
            text = fence + str(data.get('language', '')) + '\n' + text + '\n' + fence
        elif kind == 'divider':
            text = '---'
        elif kind in ('image', 'file', 'video', 'audio', 'link_preview', 'bookmark'):
            # ponytail: preserve references; downloading binaries/OCR is a separate ingestion task.
            text += '\n' + json.dumps(data, ensure_ascii=False)
        elif not external and kind not in ('page', 'paragraph', 'toggle_list') and data:
            text += '\n' + json.dumps(data, ensure_ascii=False)
        if text.strip():
            output.append(text)
        children_key = block.get('children')
        children = meta['children_map'].get(children_key, [])
        stack.extend((child, depth + 1) for child in reversed(children))
    return '\n\n'.join(output)


def quick_markdown(value):
    """Quick notes use editor blocks containing Quill delta inserts."""
    if isinstance(value, list):
        return ''.join(quick_markdown(item) for item in value)
    if not isinstance(value, dict):
        return ''
    if 'insert' in value:
        return value['insert'] if isinstance(value['insert'], str) else json.dumps(value['insert'], ensure_ascii=False)
    text = ''.join(quick_markdown(value[key]) for key in ('delta', 'attributes', 'data', 'children', 'document', 'ops') if key in value)
    return text + ('\n' if value.get('type') else '')


def note_parts(wid, kind, oid, title, body, location='', view_id=None, owner_id=None):
    title = str(title or '未命名笔记').strip()
    # WeKnora's title validator rejects HTML/control characters and long titles.
    title = re.sub(r'[<>\x00-\x1f]', '', title)[:180] or '未命名笔记'
    source = f'ponynotes://open?viewId={view_id or oid}&workspaceId={wid}'
    header = f'来源：小马笔记\n\n目录：{location}\n\n'
    if kind == 'quick':
        if not owner_id:
            raise ValueError('Quick note owner is required')
        header += f'所属用户：{owner_id}\n\n'
    else:
        header += f'[在小马笔记中打开]({source})\n\n'
    # Reserve room for provenance; split by Unicode characters, never discard a tail.
    size = 190000 - len(header)
    if size < 1:
        raise ValueError('Note path exceeds manual content limit')
    chunks = [body[i:i + size] for i in range(0, len(body), size)] or ['（空白笔记）']
    parts = {}
    for index, chunk in enumerate(chunks):
        identity = f'{wid}/{kind}/{oid}/{index}'
        if view_id is not None:
            # Linked database views can have different visibility. Retain each
            # source anchor so one member's private view cannot hide another's.
            identity += '/' + view_id
        channel = 'pn:' + hashlib.sha256(identity.encode()).hexdigest()[:40]
        label = title + (f'（{index + 1}/{len(chunks)}）' if len(chunks) > 1 else '')
        parts[channel] = {'title': label, 'content': header + chunk}
    return parts


class Cloud:
    def __init__(self, user, secret):
        self.user, self.secret = user, secret

    def get(self, path):
        # Mint a short-lived ordinary user token; the cloud still enforces access control.
        return request('http://127.0.0.1', '/api/' + path,
            headers={'Authorization': 'Bearer ' + jwt(self.user, self.secret)})['data']

    def scan(self, wid):
        base = f'workspace/{wid}'
        queue, views, paths = deque([(wid, '')]), {}, {}
        while queue:
            oid, parent = queue.popleft()
            if oid in views:
                raise ValueError('Repeated/cyclic folder view')
            # depth=1 requests at each node avoid the API's ten-level ceiling.
            node = self.get(base + '/folder?' + urllib.parse.urlencode({'depth': 1, 'root_view_id': oid}))
            if node['view_id'] != oid:
                raise ValueError('Folder root mismatch')
            views[oid] = node
            paths[oid] = parent + (' / ' if parent else '') + node['name']
            queue.extend((child['view_id'], paths[oid]) for child in node['children'])
        notes, counts = {}, Counter()
        for oid, view in views.items():
            if oid == wid or view['is_space']:
                continue
            if view['layout'] == 0:
                data = self.get(f'workspace/v1/{wid}/collab/{oid}/json?collab_type=0')
                body = document_markdown(data['collab']['document'])
                notes.update(note_parts(wid, 'document', oid, view['name'], body, paths[oid]))
                counts['documents'] += 1
            elif view['layout'] == 4:
                counts['chat_sessions_excluded'] += 1
            elif view['layout'] not in (1, 2, 3):
                raise ValueError('Unsupported cloud view layout')
        databases = self.get(base + '/database')
        db_views = {oid for oid, view in views.items() if view['layout'] in (1, 2, 3)}
        covered = set()
        for database in databases:
            allowed = sorted((v for v in database['views'] if v['view_id'] in db_views), key=lambda v: v['view_id'])
            if not allowed:
                continue
            covered.update(v['view_id'] for v in allowed)
            anchor = allowed[0]['view_id']
            dbid = database['id']
            dbbase = base + '/database/' + dbid
            fields = sorted(self.get(dbbase + '/fields'), key=lambda f: (f['name'], f['id']))
            primary = next((f['name'] for f in fields if f['is_primary']), None)
            row_ids = [r['id'] for r in self.get(dbbase + '/row')]
            if len(set(row_ids)) != len(row_ids):
                raise ValueError('Duplicate database row IDs')
            notes.update(note_parts(wid, 'database', dbid, views[anchor]['name'],
                '\n'.join('- ' + f['name'] + ' (' + f['field_type'] + ')' for f in fields), paths[anchor], anchor))
            counts['databases'] += 1
            for start in range(0, len(row_ids), 40):
                batch = row_ids[start:start + 40]
                rows = self.get(dbbase + '/row/detail?' + urllib.parse.urlencode({'ids': ','.join(batch), 'with_doc': 'true'}))
                if sorted(r['id'] for r in rows) != sorted(batch):
                    raise ValueError('Incomplete database row response; no pruning allowed')
                for row in rows:
                    if row['has_doc'] and row.get('doc') is None:
                        raise ValueError('Missing database row document; no pruning allowed')
                    display = lambda x: x if isinstance(x, str) else json.dumps(x, ensure_ascii=False, sort_keys=True)
                    body = '\n\n'.join(str(k) + '：' + display(v) for k, v in sorted(row['cells'].items()))
                    body += '\n\n' + (row.get('doc') or '')
                    label = display(row['cells'].get(primary, '数据库记录'))
                    notes.update(note_parts(wid, 'row', row['id'], label, body, paths[anchor], anchor))
                    counts['database_rows'] += 1
        if covered != db_views:
            raise ValueError('Database view missing from database listing')
        offset, seen = 0, set()
        while True:
            page = self.get(base + f'/quick-note?offset={offset}&limit=100')
            for note in page['quick_notes']:
                if note['id'] in seen:
                    raise ValueError('Unstable quick-note pagination; no pruning allowed')
                seen.add(note['id'])
                body = quick_markdown(note['data'])
                if not body.strip() and note['data']:
                    raise ValueError('Unsupported quick-note format')
                notes.update(note_parts(wid, 'quick', note['id'], '随手记 ' + note['created_at'][:10],
                    body, '随手记', owner_id=self.user['uuid']))
                counts['quick_notes'] += 1
            if not page['has_more']:
                break
            if not page['quick_notes']:
                raise ValueError('Empty quick-note page with has_more')
            offset += len(page['quick_notes'])
        return notes, dict(counts)


class WeKnora:
    def __init__(self, token=None, key=None):
        self.headers = {'X-API-Key': key} if key else {'Authorization': 'Bearer ' + token} if token else {}

    def call(self, path, method='GET', payload=None):
        return request('http://127.0.0.1:18081/api/v1/', path, method, payload, self.headers)

    def entries(self, kb):
        result, seen, page = [], set(), 1
        while True:
            response = self.call(f'knowledge-bases/{kb}/knowledge?page={page}&page_size=100')
            batch = response['data'] or []
            for item in batch:
                if item['id'] in seen:
                    raise ValueError('Unstable WeKnora pagination')
                seen.add(item['id'])
                result.append(item)
            if len(result) == response['total']:
                return result
            if not batch or len(result) > response['total']:
                raise ValueError('Incomplete WeKnora listing')
            page += 1


def preferred_model_id(models, model_type):
    candidates = [model for model in models if model['type'] == model_type
        and model.get('status', 'active') == 'active']
    defaults = [model for model in candidates if model.get('is_default')]
    choices = defaults or candidates
    return choices[0]['id'] if len(choices) == 1 else ''


def provision(state, persist):
    # Use the existing server-only administrator tenant. Never put a member's
    # private pages into another member's directly accessible personal tenant.
    credentials = json.loads((ROOT / 'admin-credentials.json').read_text())
    login = WeKnora().call('auth/login', 'POST', credentials)
    record = state.setdefault('service', {'tenant_id': login['active_tenant']['id']})
    if record['tenant_id'] != login['active_tenant']['id']:
        raise ValueError('Workspace service tenant mismatch')
    client = WeKnora(token=login['token'])
    path = f"tenants/{record['tenant_id']}/api-keys"
    keys = client.call(path)['data'] or []
    key = next((k for k in keys if k['name'] == 'PonyNotes workspaces'), None)
    known_key = record.get('api_key') if key else None
    scope = {'name': 'PonyNotes workspaces', 'full_access': False, 'knowledge_base_ids': [],
        'capabilities': ['retrieve', 'ingest', 'manage_kbs', 'manage_datasources']}
    if key is None:
        key = client.call(path, 'POST', scope)['data']
    elif (key.get('full_access') or key.get('knowledge_base_ids')
            or set(key.get('capabilities') or []) != set(scope['capabilities'])):
        key = client.call(path + '/' + str(key['id']), 'PUT', scope)['data']
    record['api_key'] = known_key or key.get('token') or key['api_key']
    models = client.call('models')['data'] or []
    embedding_models = [model for model in models
        if not model.get('description', '').startswith('PonyNotes workspace embedding ')]
    # An explicit default or a single embedding model is unambiguous. Existing
    # knowledge bases keep their own model bindings when this default changes.
    record['embedding_model_id'] = preferred_model_id(embedding_models, 'Embedding')
    # WeKnora sends card clicks to settings until both model bindings are present.
    record['summary_model_id'] = preferred_model_id(models, 'KnowledgeQA')
    persist()
    return record, WeKnora(key=record['api_key'])


def workspace_embedding(state, wid, mapping, persist, source_id):
    if not source_id:
        return ''
    proxy = mapping.get('embedding_proxy')
    if proxy and proxy.get('ready'):
        return proxy['id']
    runtime = json.loads((ROOT / 'library/runtime.json').read_text())
    credentials = json.loads((ROOT / 'admin-credentials.json').read_text())
    login = WeKnora().call('auth/login', 'POST', credentials)
    if login['active_tenant']['id'] != state['service']['tenant_id']:
        raise ValueError('Workspace service tenant mismatch')
    admin = WeKnora(token=login['token'])
    if not proxy:
        source = admin.call('models/' + source_id)['data']
        if source['type'] != 'Embedding' or source['source'] != 'local':
            raise ValueError('Workspace token accounting currently requires the configured local embedding model')
        mapping['embedding_proxy'] = {'token': secrets.token_urlsafe(32), 'name': source['name'],
            'parameters': source['parameters']['embedding_parameters'], 'source_id': source_id, 'ready': False}
        persist()  # Keep the secret before creation so lost responses are recoverable.
    proxy = mapping['embedding_proxy']
    parameters = proxy['parameters']
    description = 'PonyNotes workspace embedding ' + wid
    models = [model for model in admin.call('models')['data'] or [] if model.get('description') == description]
    if len(models) > 1:
        raise ValueError('Duplicate workspace embedding models')
    if models:
        model = models[0]
    else:
        model = admin.call('models', 'POST', {'name': proxy['name'], 'type': 'Embedding',
            'source': 'remote', 'description': description,
            'parameters': {'base_url': runtime['embedding_url'] + '/' + wid + '/v1',
                'api_key': proxy['token'], 'provider': 'generic', 'embedding_parameters': parameters}})['data']
    if (model['parameters']['base_url'] != runtime['embedding_url'] + '/' + wid + '/v1'
            or model['name'] != proxy['name'] or model['type'] != 'Embedding' or model['source'] != 'remote'
            or model['parameters']['embedding_parameters'] != parameters):
        raise ValueError('Workspace embedding configuration mismatch')
    proxy.update(id=model['id'], ready=True)
    persist()
    return proxy['id']


def workspace_mapping(state, workspace, persist):
    wid = workspace['workspace_id']
    mappings = state.setdefault('workspaces', {})
    if wid not in mappings:
        mappings[wid] = {'id': str(uuid.uuid5(uuid.NAMESPACE_URL, f'ponynotes:workspace:{wid}')),
            'notes': {}, 'ready': False}
        persist()
    return mappings[wid]


def reconcile(client, kb, desired, cache, persist):
    """Recover create timeouts by the immutable source channel, not local IDs."""
    existing, duplicates = {}, []
    for item in client.entries(kb['id']):
        channel = item.get('channel', '')
        if not CHANNEL.fullmatch(channel) or item.get('parse_status') == 'deleting':
            continue
        if channel in existing:
            duplicates.append(item)
        else:
            existing[channel] = item
    counts = Counter()
    status = 'publish' if kb.get('embedding_model_id') else 'draft'
    for channel, content in desired.items():
        payload = {**content, 'status': status}
        source_hash = digest(payload)
        item = existing.get(channel)
        saved = cache.get(channel, {})
        if item and saved.get('source') == source_hash and saved.get('remote') == digest([item.get('title'), item.get('metadata')]) and item.get('parse_status') != 'failed':
            counts['unchanged'] += 1
        else:
            if item:
                item = client.call('knowledge/manual/' + item['id'], 'PUT', payload)['data']
                counts['updated'] += 1
            else:
                item = client.call(f"knowledge-bases/{kb['id']}/knowledge/manual", 'POST', {**payload, 'channel': channel})['data']
                counts['created'] += 1
            cache[channel] = {'source': source_hash, 'remote': digest([item.get('title'), item.get('metadata')])}
            persist()
        counts[item['parse_status']] += 1
    # Called only after a complete source scan AND all writes succeed.
    stale = [item for channel, item in existing.items() if channel not in desired] + duplicates
    for item in stale:
        client.call('knowledge/' + item['id'], 'DELETE')
        if item['channel'] not in desired:
            cache.pop(item['channel'], None)
        counts['deleted'] += 1
        persist()
    return dict(counts)


def sync(state, persist, users, workspaces, secret, scan_only=False):
    report = {'at': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()), 'users': len(users), 'workspaces': [], 'errors': []}
    active = {user['uuid']: user for user in users}
    workspace_ids = {workspace['workspace_id'] for workspace in workspaces}
    if len(workspace_ids) != len(workspaces):
        raise ValueError('Duplicate workspace inventory')
    service, client = None, None
    for workspace in workspaces:
        wid = workspace['workspace_id']
        try:
            members = sorted(set(workspace['members']), key=lambda uid: (uid != workspace['owner_uuid'], uid))
            if not members:
                raise ValueError('Workspace has no active members; preserving knowledge base')
            desired, coverage = {}, Counter()
            # A complete scan of every member must succeed before any pruning.
            # ponytail: scan members serially; deduplicate reads if this outgrows the five-minute interval.
            for uid in members:
                notes, counts = Cloud(active[uid], secret).scan(wid)
                for channel, content in notes.items():
                    desired.setdefault(channel, content)
                coverage.update(counts)
            result = {'workspace': wid, 'members': len(members), 'parts': len(desired), 'coverage': dict(coverage)}
            if not scan_only:
                mapping = workspace_mapping(state, workspace, persist)
                if client is None:
                    service, client = provision(state, persist)
                path = 'knowledge-bases/' + mapping['id']
                try:
                    kb = client.call(path)['data']
                except APIError as error:
                    if error.status != 404:
                        raise
                    kb = None
                source_id = (kb or {}).get('embedding_model_id') or service.get('embedding_model_id', '')
                if not mapping.get('embedding_proxy'):
                    mapping['ready'] = False
                    persist()
                embedding_id = workspace_embedding(state, wid, mapping, persist, source_id)
                if kb is None:
                    kb = client.call('knowledge-bases', 'POST', {'id': mapping['id'],
                        'name': '小马笔记 · ' + (workspace.get('workspace_name') or wid),
                        'type': 'document', 'description': '工作空间知识库；小马云端笔记自动同步。',
                        'embedding_model_id': embedding_id,
                        'summary_model_id': service.get('summary_model_id', '')})['data']
                else:
                    updates = {}
                    if kb.get('embedding_model_id', '') != embedding_id:
                        if kb.get('embedding_model_id') and client.entries(kb['id']):
                            raise ValueError('Existing workspace KB must be reindexed before switching its embedding model')
                        updates['embedding_model_id'] = embedding_id
                    if not kb.get('summary_model_id') and service.get('summary_model_id'):
                        updates['summary_model_id'] = service['summary_model_id']
                    if updates:
                        kb = client.call(path, 'PATCH', updates)['data']
                result['sync'] = reconcile(client, kb, desired, mapping['notes'], persist)
                mapping['ready'] = True
                persist()
            report['workspaces'].append(result)
            print(json.dumps(result, ensure_ascii=False), flush=True)
        except Exception as error:
            report['errors'].append({'workspace': wid, 'error': safe_error(error)})
    if not scan_only:
        for wid, mapping in list(state.get('workspaces', {}).items()):
            if wid in workspace_ids:
                continue
            try:
                if client is None:
                    service, client = provision(state, persist)
                remove_workspace(client, state['workspaces'], wid, persist)
            except Exception as error:
                report['errors'].append({'workspace': wid, 'error': safe_error(error)})
    return report


def safe_error(error):
    # Do not expose source content, credentials, URLs with query parameters, or HTTP bodies.
    return str(error) if isinstance(error, APIError) else type(error).__name__


def remove_workspace(client, mappings, wid, persist):
    try:
        client.call('knowledge-bases/' + mappings[wid]['id'], 'DELETE')
    except APIError as error:
        if error.status != 404:
            raise
    del mappings[wid]
    persist()


def install():
    if Path(__file__).resolve().parent != ROOT:
        raise ValueError('Install the script under ' + str(ROOT))
    unit = '[Unit]\nDescription=Sync PonyNotes cloud notes to WeKnora\nAfter=docker.service network-online.target\n\n[Service]\nType=oneshot\nUMask=0077\nExecStart=/usr/bin/python3 -u /opt/ponynotes-weknora/sync-weknora.py\nTimeoutStartSec=30min\n'
    timer = '[Unit]\nDescription=Sync PonyNotes notes every five minutes\n\n[Timer]\nOnBootSec=2min\nOnUnitInactiveSec=5min\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n'
    Path('/etc/systemd/system/ponynotes-weknora-sync.service').write_text(unit)
    Path('/etc/systemd/system/ponynotes-weknora-sync.timer').write_text(timer)
    subprocess.run(['systemctl', 'daemon-reload'], check=True)
    subprocess.run(['systemctl', 'enable', '--now', 'ponynotes-weknora-sync.timer'], check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--scan-only', action='store_true', help='Read sources, make no WeKnora changes')
    parser.add_argument('--install', action='store_true', help='Install the five-minute systemd timer')
    args = parser.parse_args()
    os.umask(0o077)
    if args.install:
        install()
        return
    import fcntl
    directory = ROOT / 'sync'
    directory.mkdir(mode=0o700, exist_ok=True)
    directory.chmod(0o700)
    with open(directory / 'lock', 'w') as lock:
        # ponytail: one full scan at a time; use an event queue when scans exceed five minutes.
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        path = directory / 'state.json'
        state = json.loads(path.read_text()) if path.exists() else {'users': {}}
        if not args.scan_only and 'workspaces' not in state:
            backup = directory / 'state.before-workspace-kbs.json'
            if path.exists() and not backup.exists():
                save(backup, state)
            state['workspaces'] = {}
            save(path, state)
        report = sync(state, lambda: save(path, state), cloud_users(), cloud_workspaces(), cloud_secret(), args.scan_only)
        save(directory / 'last-run.json', report)
        print(json.dumps({'errors': report['errors'], 'workspaces': len(report['workspaces'])}, ensure_ascii=False))
        if report['errors']:
            raise SystemExit(1)


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        print('Sync failed: ' + safe_error(error))
        raise SystemExit(1)
