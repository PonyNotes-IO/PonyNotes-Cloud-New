"""Authenticated note retrieval for PonyNotes clients. No WeKnora keys leave the server."""
import argparse
from concurrent.futures import ThreadPoolExecutor
from http.server import BaseHTTPRequestHandler, HTTPServer
import importlib.util
import hmac
import json
import math
from pathlib import Path
import re
import shutil
import subprocess
import threading
import urllib.parse
import uuid

spec = importlib.util.spec_from_file_location('sync_notes', Path(__file__).with_name('sync-weknora.py'))
sync = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sync)

# ponytail: one bridge process owns settings; use a database if this service gains replicas.
settings_lock = threading.Lock()


def settings():
    path = sync.ROOT / 'library/settings.json'
    return json.loads(path.read_text()) if path.exists() else {}


def session(authorization):
    if not authorization.startswith('Bearer ') or len(authorization) > 16384:
        raise sync.APIError(401)
    user = sync.request('http://127.0.0.1', '/gotrue/user', headers={'Authorization': authorization})
    cloud = sync.Cloud.__new__(sync.Cloud)
    cloud.get = lambda path: sync.request('http://127.0.0.1/api/', path,
        headers={'Authorization': authorization})['data']
    profile = cloud.get('user/profile')
    if profile['uuid'] != user['id']:
        raise sync.APIError(401)
    return user['id'], profile['uid'], cloud, cloud.get('workspace')


def workspace_id(payload):
    try:
        return str(uuid.UUID(payload.get('workspace_id', '')))
    except (ValueError, TypeError, AttributeError):
        raise sync.APIError(400) from None


def base_info(workspace, uid, owner_uid, config, state):
    wid = workspace['workspace_id']
    visibility = config.get('workspaces', {}).get(wid, {}).get('visibility', 'shared')
    can_manage = workspace['owner_uid'] == owner_uid
    if visibility != 'shared' and not can_manage:
        return None
    return {'workspace_id': wid, 'name': workspace['workspace_name'],
        'visibility': visibility, 'can_manage': can_manage,
        'is_favorite': wid in config.get('favorites', {}).get(uid, []),
        'ready': bool(state.get('workspaces', {}).get(wid, {}).get('ready'))}


def accessible_base(wid, uid, owner_uid, workspaces):
    workspace = next((w for w in workspaces if w['workspace_id'] == wid), None)
    info = base_info(workspace, uid, owner_uid, settings(), {}) if workspace else None
    if info is None:
        raise sync.APIError(403)
    return info


def sync_state():
    path = sync.ROOT / 'sync/state.json'
    return json.loads(path.read_text()) if path.exists() else {}


def embedding(authorization, wid, payload):
    wid = workspace_id({'workspace_id': wid})
    proxy = sync_state().get('workspaces', {}).get(wid, {}).get('embedding_proxy', {})
    expected = 'Bearer ' + proxy.get('token', '')
    if not proxy.get('token') or not hmac.compare_digest(authorization, expected):
        raise sync.APIError(401)
    texts = payload.get('input')
    if isinstance(texts, str):
        texts = [texts]
    if (payload.get('model') != proxy['name'] or not isinstance(texts, list)
            or not 1 <= len(texts) <= 100 or any(not isinstance(text, str) for text in texts)
            or sum(map(len, texts)) > 1000000):
        raise sync.APIError(400)
    billing = sync.billing_authorization(wid)
    parameters = proxy['parameters']
    request = {'model': proxy['name'], 'input': texts, 'truncate': True,
        'options': {'num_ctx': parameters.get('truncate_prompt_tokens') or 511}}
    if parameters.get('supports_dimension_override'):
        request['dimensions'] = parameters['dimension']
    # Only the configured local runtime is reachable; no client-supplied URLs or model keys.
    response = sync.request('http://127.0.0.1:11434', '/api/embed', 'POST', request)
    tokens = response.get('prompt_eval_count')
    if type(tokens) is not int or not 0 <= tokens <= 2**63 - 1:
        raise sync.APIError(503)
    sync.record_tokens(billing, tokens)
    vectors = response.get('embeddings')
    if (not isinstance(vectors, list) or len(vectors) != len(texts)
            or any(not isinstance(vector, list) or len(vector) != parameters['dimension']
                or any(type(value) not in (int, float) or not math.isfinite(value) for value in vector)
                for vector in vectors)):
        raise sync.APIError(503)
    return {'object': 'list', 'model': proxy['name'], 'data': [
        {'object': 'embedding', 'index': index, 'embedding': vector} for index, vector in enumerate(vectors)],
        'usage': {'prompt_tokens': tokens, 'total_tokens': tokens}}


def entry_source(entry, kb, wid, uid, visible):
    if entry['knowledge_base_id'] != kb:
        return None
    content = (entry.get('metadata') or {}).get('content', '')
    if not sync.CHANNEL.fullmatch(entry.get('channel', '')):
        # Workspace uploads/connectors inherit base access; mirrored Cloud notes
        # keep their per-page/owner checks even if their channel was changed.
        if entry.get('type') not in ('file', 'url', 'manual') or content.startswith('来源：小马笔记\n\n'):
            return None
        return {'title': entry['title'], 'url': ''}
    if content.startswith('来源：小马笔记\n\n目录：随手记\n\n'):
        if not content.startswith(f'来源：小马笔记\n\n目录：随手记\n\n所属用户：{uid}\n\n'):
            return None
        url = ''
    else:
        link = re.search(r'ponynotes://open\?viewId=([0-9a-f-]+)&workspaceId=([0-9a-f-]+)', content)
        if not link or link[2] != wid or link[1] not in visible:
            return None
        url = link[0]
    return {'title': entry['title'], 'url': url}


def wiki_readable(page, kb, allowed, owner):
    refs = page.get('source_refs') or []
    return page.get('knowledge_base_id') == kb and (
        all(ref.split('|', 1)[0] in allowed for ref in refs) if refs else owner)


def wiki_pages(client, kb):
    pages, seen, number = [], set(), 1
    while True:
        result = client.call(f'knowledgebase/{kb}/wiki/pages?page={number}&page_size=100')
        batch = result['pages'] or []
        for page in batch:
            if page['slug'] in seen:
                raise ValueError('Unstable wiki pagination')
            seen.add(page['slug'])
            pages.append(page)
        if len(pages) == result['total']:
            return pages
        if not batch or len(pages) > result['total']:
            raise ValueError('Incomplete wiki listing')
        number += 1


def library(authorization, action, payload):
    uid, owner_uid, cloud, workspaces = session(authorization)
    if action == 'bases':
        config, state = settings(), sync_state()
        return {'bases': [info for workspace in workspaces
            if (info := base_info(workspace, uid, owner_uid, config, state)) is not None]}
    wid = workspace_id(payload)
    info = accessible_base(wid, uid, owner_uid, workspaces)
    if action in ('visibility', 'favorite'):
        if action == 'visibility':
            if not info['can_manage']:
                raise sync.APIError(403)
            if payload.get('visibility') not in ('private', 'shared'):
                raise sync.APIError(400)
        elif not isinstance(payload.get('is_favorite'), bool):
            raise sync.APIError(400)
        with settings_lock:
            config = settings()
            if action == 'visibility':
                config.setdefault('workspaces', {}).setdefault(wid, {})['visibility'] = payload['visibility']
            else:
                favorites = set(config.setdefault('favorites', {}).get(uid, []))
                if payload['is_favorite']:
                    favorites.add(wid)
                else:
                    favorites.discard(wid)
                config['favorites'][uid] = sorted(favorites)
            path = sync.ROOT / 'library/settings.json'
            path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            sync.save(path, config)
        return {'updated': True}
    if action not in ('documents', 'document', 'sources', 'wiki', 'wiki_page'):
        raise sync.APIError(404)
    state = sync_state()
    mapping = state.get('workspaces', {}).get(wid)
    if not mapping or not mapping.get('ready'):
        return {'documents': [], 'sources': [], 'pages': [], 'status': 'awaiting_sync'}
    client = sync.WeKnora(key=state['service']['api_key'])
    if action == 'sources':
        sources = client.call('datasource?' + urllib.parse.urlencode({'kb_id': mapping['id']})) or []
        # Never forward connector credentials, settings, cursors or raw sync logs.
        fields = ('id', 'name', 'type', 'status', 'last_sync_at', 'total_items_synced')
        return {'sources': [{key: source.get(key) for key in fields} for source in sources
            if source.get('knowledge_base_id') == mapping['id']], 'status': 'ready'}
    visible = visible_views(cloud, wid)
    if action in ('wiki', 'wiki_page'):
        kb = client.call('knowledge-bases/' + mapping['id'])['data']
        if not (kb.get('indexing_strategy') or {}).get('wiki_enabled'):
            if action == 'wiki_page':
                raise sync.APIError(404)
            return {'pages': [], 'status': 'ready'}
        allowed = {item['id'] for item in client.entries(mapping['id'])
            if entry_source(item, mapping['id'], wid, uid, visible) is not None}
        if action == 'wiki_page':
            slug = payload.get('slug')
            if not isinstance(slug, str) or not 1 <= len(slug) <= 255 or any(
                    part in ('', '.', '..') for part in slug.split('/')) or re.search(r'[\\\x00-\x1f\x7f]', slug):
                raise sync.APIError(400)
            page = client.call('knowledgebase/' + mapping['id'] + '/wiki/pages/' + urllib.parse.quote(slug, safe='/'))
            if not wiki_readable(page, mapping['id'], allowed, info['can_manage']):
                raise sync.APIError(403)
            content = page.get('content') or ''
            return {'title': page['title'], 'content': content[:200000], 'truncated': len(content) > 200000}
        pages = [page for page in wiki_pages(client, mapping['id'])
            if wiki_readable(page, mapping['id'], allowed, info['can_manage'])]
        slugs = {page['slug'] for page in pages}
        return {'pages': [{'id': page['slug'], 'title': page['title'],
            'summary': page.get('summary', ''), 'status': page.get('status', ''),
            'out_links': [slug for slug in page.get('out_links') or [] if slug in slugs]}
            for page in pages], 'status': 'ready'}
    if action == 'document':
        kid = payload.get('document_id', '')
        try:
            kid = str(uuid.UUID(kid))
        except (ValueError, TypeError, AttributeError):
            raise sync.APIError(400) from None
        entry = client.call('knowledge/' + kid)['data']
        source = entry_source(entry, mapping['id'], wid, uid, visible)
        if source is None:
            raise sync.APIError(403)
        content = (entry.get('metadata') or {}).get('content', '')
        if content.startswith('来源：小马笔记\n\n'):
            content = content.split('\n\n', 3)[-1]
        return {**source, 'content': content[:200000], 'truncated': len(content) > 200000}
    documents = []
    for item in client.entries(mapping['id']):
        source = entry_source(item, mapping['id'], wid, uid, visible)
        if source is not None:
            documents.append({**source, 'id': item['id'], 'status': item.get('parse_status', '')})
    return {'documents': documents, 'status': 'ready'}


def visible_views(cloud, wid):
    pending, visible = [wid], set()
    while pending:
        oid = pending.pop()
        if oid in visible:
            raise ValueError('Cyclic folder')
        node = cloud.get(f'workspace/{wid}/folder?depth=1&root_view_id={oid}')
        if node['view_id'] != oid:
            raise ValueError('Folder mismatch')
        visible.add(oid)
        pending.extend(c['view_id'] for c in node['children'])
    return visible


def search(authorization, payload):
    if not authorization.startswith('Bearer ') or len(authorization) > 16384:
        raise sync.APIError(401)
    query = payload.get('query')
    if not isinstance(query, str) or not 1 <= len(query.strip()) <= 2000:
        raise sync.APIError(400)
    wid = workspace_id(payload)
    # Validate the actual login session, never trust a client-supplied user/tenant/KB ID.
    uid, owner_uid, cloud, workspaces = session(authorization)
    accessible_base(wid, uid, owner_uid, workspaces)
    # Recheck visibility at query time so removed private pages cannot be retrieved.
    visible = visible_views(cloud, wid)
    state = sync_state()
    mapping = state.get('workspaces', {}).get(wid)
    if not mapping or not mapping.get('ready'):
        return {'sources': [], 'status': 'awaiting_sync'}
    client = sync.WeKnora(key=state['service']['api_key'])
    kb = client.call('knowledge-bases/' + mapping['id'])['data']
    if not kb.get('embedding_model_id'):
        raise sync.APIError(503)
    hits = client.call('knowledge-bases/' + kb['id'] + '/hybrid-search', 'POST',
        {'query_text': query.strip(), 'match_count': 12, 'vector_threshold': 0.35})['data'] or []
    sources, seen, budget = [], set(), 16000
    for hit in hits:
        kid = hit.get('knowledge_id')
        if not kid or kid in seen:
            continue
        entry = client.call('knowledge/' + kid)['data']
        source = entry_source(entry, kb['id'], wid, uid, visible)
        if source is None:
            continue
        text = hit.get('content') or ''
        if not isinstance(text, str) or not text.strip():
            continue
        text = text[:min(3500, budget)]
        sources.append({**source, 'content': text})
        budget -= len(text)
        seen.add(kid)
        if len(sources) >= 5 or budget <= 0:
            break
    return {'sources': sources, 'status': 'matched' if sources else 'no_match'}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass  # Do not log bearer tokens, queries, or note text.

    def do_POST(self):
        try:
            internal = re.fullmatch(r'/internal/knowledge/([0-9a-f-]+)/v1/embeddings', self.path)
            if bool(internal) != self.server.internal:
                raise sync.APIError(404)
            action = self.path.removeprefix('/api/knowledge/')
            if not internal and (self.path != '/api/knowledge/' + action or action not in (
                    'search', 'bases', 'visibility', 'favorite', 'documents', 'document',
                    'sources', 'wiki', 'wiki_page')):
                raise sync.APIError(404)
            length = int(self.headers.get('Content-Length', '0'))
            if not 0 < length <= (8000000 if internal else 16384):
                raise sync.APIError(400)
            self.connection.settimeout(15)
            payload = json.loads(self.rfile.read(length))
            if not isinstance(payload, dict):
                raise sync.APIError(400)
            authorization = self.headers.get('Authorization', '')
            if internal:
                self.respond(200, embedding(authorization, internal[1], payload))
            else:
                result = search(authorization, payload) if action == 'search' else library(authorization, action, payload)
                self.respond(200, {'success': True, 'data': result})
        except sync.APIError as error:
            status = error.status if error.status in (400, 401, 403, 404) else 503
            messages = {400: '知识库参数错误。', 401: '登录已过期，请重新登录。',
                403: '你没有此知识库的访问或管理权限。', 404: '知识库接口不存在。'}
            self.respond(status, {'success': False, 'message': messages.get(status, '知识库暂时不可用，请稍后重试。')})
        except (ValueError, TypeError):
            self.respond(400, {'success': False, 'message': '检索参数错误。'})
        except Exception:
            self.respond(503, {'success': False, 'message': '知识库检索暂时不可用，请稍后重试。'})

    def respond(self, status, payload):
        body = json.dumps(payload, ensure_ascii=False).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json; charset=utf-8')
        self.send_header('Cache-Control', 'no-store')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class Server(HTTPServer):
    # ponytail: bounded blocking workers; use the main cloud service if request volume outgrows this bridge.
    def __init__(self, address, internal=False):
        self.internal = internal
        super().__init__(address, Handler)
        self.pool = ThreadPoolExecutor(max_workers=8)

    def process_request(self, connection, address):
        self.pool.submit(self.finish, connection, address)

    def finish(self, connection, address):
        try:
            self.finish_request(connection, address)
        finally:
            self.shutdown_request(connection)


def install():
    (sync.ROOT / 'library').mkdir(mode=0o700, parents=True, exist_ok=True)
    (sync.ROOT / 'library').chmod(0o700)
    network = json.loads(subprocess.check_output(['docker', 'network', 'inspect', 'ponynotes_default']))[0]
    gateway = network['IPAM']['Config'][0]['Gateway']
    sync.save(sync.ROOT / 'library/runtime.json', {'embedding_url': f'http://{gateway}:18083/internal/knowledge'})
    compose = ['docker', 'compose', '-p', 'ponynotes-weknora', '-f', 'compose.yaml', '-f', 'compose.local.json']
    container_id = subprocess.check_output(compose + ['ps', '-q', 'app'], cwd=sync.ROOT).decode().strip()
    container = json.loads(subprocess.check_output(['docker', 'inspect', container_id]))[0]
    if shutil.which('ufw'):
        bridge = network.get('Options', {}).get('com.docker.network.bridge.name') or 'br-' + network['Id'][:12]
        subnet = network['IPAM']['Config'][0]['Subnet']
        subprocess.run(['ufw', 'allow', 'in', 'on', bridge, 'proto', 'tcp', 'from', subnet,
            'to', gateway, 'port', '18082', 'comment', 'PonyNotes private knowledge search'], check=True)
        for network_name in container['NetworkSettings']['Networks']:
            embedding_network = json.loads(subprocess.check_output(['docker', 'network', 'inspect', network_name]))[0]
            embedding_bridge = embedding_network.get('Options', {}).get('com.docker.network.bridge.name') or 'br-' + embedding_network['Id'][:12]
            subprocess.run(['ufw', 'allow', 'in', 'on', embedding_bridge, 'proto', 'tcp', 'from',
                embedding_network['IPAM']['Config'][0]['Subnet'], 'to', gateway, 'port', '18083',
                'comment', 'PonyNotes private embedding accounting'], check=True)
    # Bind only to the private Docker bridge, reached through the existing authenticated route.
    service = f'''[Unit]
Description=PonyNotes private knowledge retrieval
After=docker.service network-online.target

[Service]
UMask=0077
ExecStart=/usr/bin/python3 -u /opt/ponynotes-weknora/search-weknora.py --bind {gateway}
Restart=on-failure
RestartSec=5
NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths=/opt/ponynotes-weknora/library
ProtectHome=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
'''
    Path('/etc/systemd/system/ponynotes-weknora-search.service').write_text(service)
    subprocess.run(['systemctl', 'daemon-reload'], check=True)
    subprocess.run(['systemctl', 'enable', '--now', 'ponynotes-weknora-search.service'], check=True)
    subprocess.run(['systemctl', 'restart', 'ponynotes-weknora-search.service'], check=True)
    nginx = Path('/opt/ponynotes/nginx.conf')
    original = nginx.read_text()
    marker = '    # PonyNotes knowledge search\n'
    block = marker + f'''    location ^~ /api/knowledge/ {{
      client_max_body_size 16k;
      proxy_pass http://{gateway}:18082;
      proxy_read_timeout 120s;
    }}
'''
    backup = nginx.with_name('nginx.conf.before-knowledge-search')
    if not backup.exists():
        backup.write_text(original)
    if marker in original:
        updated, count = re.subn(r'    # PonyNotes knowledge search\n    location [^\n]+\{\n.*?^    }\n',
            lambda _: block, original, count=1, flags=re.M | re.S)
        if count != 1:
            raise ValueError('Knowledge proxy block not recognized')
    else:
        if '    location /gotrue/ {' not in original:
            raise ValueError('Nginx insertion point missing')
        updated = original.replace('    location /gotrue/ {', block + '    location /gotrue/ {', 1)
    nginx.write_text(updated)
    try:
        subprocess.run(['docker', 'compose', 'exec', '-T', 'nginx', 'nginx', '-t'], cwd=sync.CLOUD_ROOT, check=True)
        subprocess.run(['docker', 'compose', 'exec', '-T', 'nginx', 'nginx', '-s', 'reload'], cwd=sync.CLOUD_ROOT, check=True)
    except Exception:
        nginx.write_text(original)
        raise
    # Trust only this host's bridge address, preserving all existing model URL allowlist entries.
    environment = dict(item.split('=', 1) for item in container['Config']['Env'] if '=' in item)
    whitelist = [item for item in environment.get('SSRF_WHITELIST_EXTRA', '').split(',') if item]
    if gateway not in whitelist:
        whitelist.append(gateway)
        override_path = sync.ROOT / 'compose.local.json'
        original_override = override_path.read_text()
        override = json.loads(original_override)
        override.setdefault('services', {}).setdefault('app', {}).setdefault('environment', {})['SSRF_WHITELIST_EXTRA'] = ','.join(whitelist)
        backup = override_path.with_name('compose.local.before-workspace-billing.json')
        if not backup.exists():
            backup.write_text(original_override)
            backup.chmod(0o600)
        sync.save(override_path, override)
        try:
            subprocess.run(compose + ['config', '--quiet'], cwd=sync.ROOT, check=True)
            subprocess.run(compose + ['up', '-d', '--no-deps', '--wait', '--wait-timeout', '180', 'app'],
                cwd=sync.ROOT, check=True)
        except Exception:
            override_path.write_text(original_override)
            raise


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--install', action='store_true')
    parser.add_argument('--bind', default='127.0.0.1')
    args = parser.parse_args()
    if args.install:
        install()
    else:
        # Separate workers prevent search -> WeKnora -> embedding requests from sharing a full pool.
        internal = Server((args.bind, 18083), internal=True)
        threading.Thread(target=internal.serve_forever, daemon=True).start()
        Server((args.bind, 18082)).serve_forever()
