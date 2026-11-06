"""Mirror authorized cloud notes into private per-user WeKnora tenants (stdlib only)."""
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
    if result.get('success') is False or result.get('code', 0) != 0:
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


def note_parts(wid, kind, oid, title, body, location='', view_id=None):
    title = str(title or '未命名笔记').strip()
    # WeKnora's title validator rejects HTML/control characters and long titles.
    title = re.sub(r'[<>\x00-\x1f]', '', title)[:180] or '未命名笔记'
    source = f'ponynotes://open?viewId={view_id or oid}&workspaceId={wid}'
    header = f'来源：小马笔记\n\n目录：{location}\n\n'
    if kind != 'quick':
        header += f'[在小马笔记中打开]({source})\n\n'
    # Reserve room for provenance; split by Unicode characters, never discard a tail.
    size = 190000 - len(header)
    if size < 1:
        raise ValueError('Note path exceeds manual content limit')
    chunks = [body[i:i + size] for i in range(0, len(body), size)] or ['（空白笔记）']
    parts = {}
    for index, chunk in enumerate(chunks):
        identity = f'{wid}/{kind}/{oid}/{index}'
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
                notes.update(note_parts(wid, 'quick', note['id'], '随手记 ' + note['created_at'][:10], body, '随手记'))
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


def provision(user, state, persist):
    uid = user['uuid']
    if uid not in state['users']:
        state['users'][uid] = {'email': f'pn-{uid}@cloud.ponynotes.local',
            'password': 'Pn!' + secrets.token_urlsafe(18) + '7aA', 'workspaces': {}}
        # Save password before create so a lost HTTP response is safely recoverable.
        persist()
    record = state['users'][uid]
    record['cloud_email'] = user.get('email')
    if not record.get('user_id'):
        credentials = json.loads((ROOT / 'admin-credentials.json').read_text())
        login = WeKnora().call('auth/login', 'POST', credentials)
        created = WeKnora(token=login['token']).call('system/admin/users/create', 'POST',
            {'username': 'pn-' + uid, 'email': record['email'], 'password': record['password']})['user']
        # Possession of our saved random password, not email equality, proves the mapping.
        own = WeKnora().call('auth/login', 'POST', {'email': record['email'], 'password': record['password']})
        if own['user']['id'] != created['id']:
            raise ValueError('Provisioned identity mismatch')
        record['user_id'] = created['id']
        record['tenant_id'] = own['active_tenant']['id']
        persist()
    if not record.get('api_key'):
        own = WeKnora().call('auth/login', 'POST', {'email': record['email'], 'password': record['password']})
        if own['user']['id'] != record['user_id'] or own['active_tenant']['id'] != record['tenant_id']:
            raise ValueError('Provisioned tenant mismatch')
        client = WeKnora(token=own['token'])
        path = f"tenants/{record['tenant_id']}/api-keys"
        keys = client.call(path)['data'] or []
        key = next((k for k in keys if k['name'] == 'PonyNotes sync'), None)
        if key is None:
            key = client.call(path, 'POST', {'name': 'PonyNotes sync', 'full_access': False,
                'knowledge_base_ids': [], 'capabilities': ['retrieve', 'ingest', 'manage_kbs']})['data']
        record['api_key'] = key.get('token') or key['api_key']
        persist()
    return record, WeKnora(key=record['api_key'])


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


def sync(state, persist, users, secret, scan_only=False):
    report = {'at': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()), 'users': len(users), 'workspaces': [], 'errors': []}
    active = {u['uuid'] for u in users}
    for user in users:
        uid = user['uuid']
        cloud = Cloud(user, secret)
        try:
            workspaces = cloud.get('workspace')
            record, client = (None, None) if scan_only else provision(user, state, persist)
            allowed = {w['workspace_id'] for w in workspaces}
            if record:
                for wid in list(record['workspaces']):
                    if wid not in allowed:
                        remove_workspace(client, record['workspaces'], wid, persist)
            for workspace in workspaces:
                wid = workspace['workspace_id']
                try:
                    desired, coverage = cloud.scan(wid)
                    result = {'user': uid, 'workspace': wid, 'parts': len(desired), 'coverage': coverage}
                    if not scan_only:
                        mapping = record['workspaces'].setdefault(wid, {'id': str(uuid.uuid5(uuid.NAMESPACE_URL, f'ponynotes:{uid}:{wid}')), 'notes': {}})
                        persist()
                        path = 'knowledge-bases/' + mapping['id']
                        try:
                            kb = client.call(path)['data']
                        except APIError as error:
                            if error.status != 404:
                                raise
                            kb = client.call('knowledge-bases', 'POST', {'id': mapping['id'],
                                'name': '小马笔记 · ' + (workspace.get('workspace_name') or workspace.get('name') or wid),
                                'type': 'document', 'description': '小马云端笔记自动同步；请在小马笔记中编辑原文。'})['data']
                        result['sync'] = reconcile(client, kb, desired, mapping['notes'], persist)
                    report['workspaces'].append(result)
                    print(json.dumps(result, ensure_ascii=False), flush=True)
                except Exception as error:
                    report['errors'].append({'user': uid, 'workspace': wid, 'error': safe_error(error)})
        except Exception as error:
            report['errors'].append({'user': uid, 'error': safe_error(error)})
    if not scan_only:
        for uid, record in state['users'].items():
            if uid not in active and record.get('api_key'):
                try:
                    for wid in list(record['workspaces']):
                        remove_workspace(WeKnora(key=record['api_key']), record['workspaces'], wid, persist)
                except Exception as error:
                    report['errors'].append({'user': uid, 'error': safe_error(error)})
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
        env = json.loads(subprocess.check_output(['docker', 'inspect', 'ponynotes-gotrue-1']))[0]['Config']['Env']
        secret = next(x.split('=', 1)[1] for x in env if x.startswith('GOTRUE_JWT_SECRET='))
        report = sync(state, lambda: save(path, state), cloud_users(), secret, args.scan_only)
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
