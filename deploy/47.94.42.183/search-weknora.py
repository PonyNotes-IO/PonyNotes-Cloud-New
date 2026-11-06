"""Authenticated note retrieval for PonyNotes clients. No WeKnora keys leave the server."""
import argparse
from concurrent.futures import ThreadPoolExecutor
from http.server import BaseHTTPRequestHandler, HTTPServer
import importlib.util
import json
from pathlib import Path
import re
import shutil
import subprocess
import urllib.parse
import uuid

spec = importlib.util.spec_from_file_location('sync_notes', Path(__file__).with_name('sync-weknora.py'))
sync = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sync)


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
    try:
        wid = str(uuid.UUID(payload.get('workspace_id', '')))
    except (ValueError, TypeError, AttributeError):
        raise sync.APIError(400) from None
    # Validate the actual login session, never trust a client-supplied user/tenant/KB ID.
    user = sync.request('http://127.0.0.1', '/gotrue/user', headers={'Authorization': authorization})
    uid = user['id']
    cloud = sync.Cloud.__new__(sync.Cloud)
    cloud.get = lambda path: sync.request('http://127.0.0.1/api/', path,
        headers={'Authorization': authorization})['data']
    workspaces = cloud.get('workspace')
    if wid not in {w['workspace_id'] for w in workspaces}:
        raise sync.APIError(403)
    # Recheck visibility at query time so removed private pages cannot be retrieved.
    visible = visible_views(cloud, wid)
    state = json.loads((sync.ROOT / 'sync/state.json').read_text())
    account = state['users'].get(uid)
    mapping = account and account['workspaces'].get(wid)
    if not mapping:
        return {'sources': [], 'status': 'awaiting_sync'}
    client = sync.WeKnora(key=account['api_key'])
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
        if entry['knowledge_base_id'] != kb['id'] or not sync.CHANNEL.fullmatch(entry.get('channel', '')):
            continue
        metadata = entry.get('metadata') or {}
        content = metadata.get('content', '')
        link = re.search(r'ponynotes://open\?viewId=([0-9a-f-]+)&workspaceId=([0-9a-f-]+)', content)
        if link:
            if link[2] != wid or link[1] not in visible:
                continue
            url = link[0]
        elif content.startswith('来源：小马笔记\n\n目录：随手记\n'):
            url = ''  # Quick notes have no supported document deep link.
        else:
            continue
        text = hit.get('content') or ''
        if not isinstance(text, str) or not text.strip():
            continue
        text = text[:min(3500, budget)]
        sources.append({'title': entry['title'], 'url': url, 'content': text})
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
            if self.path != '/api/knowledge/search':
                raise sync.APIError(404)
            length = int(self.headers.get('Content-Length', '0'))
            if not 0 < length <= 16384:
                raise sync.APIError(400)
            self.connection.settimeout(15)
            payload = json.loads(self.rfile.read(length))
            if not isinstance(payload, dict):
                raise sync.APIError(400)
            result = search(self.headers.get('Authorization', ''), payload)
            self.respond(200, {'success': True, 'data': result})
        except sync.APIError as error:
            status = error.status if error.status in (400, 401, 403, 404) else 503
            self.respond(status, {'success': False, 'message': '知识库检索不可用，请稍后重试或关闭知识库检索。'})
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
    def __init__(self, address):
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
    network = json.loads(subprocess.check_output(['docker', 'network', 'inspect', 'ponynotes_default']))[0]
    gateway = network['IPAM']['Config'][0]['Gateway']
    if shutil.which('ufw'):
        bridge = network.get('Options', {}).get('com.docker.network.bridge.name') or 'br-' + network['Id'][:12]
        subnet = network['IPAM']['Config'][0]['Subnet']
        subprocess.run(['ufw', 'allow', 'in', 'on', bridge, 'proto', 'tcp', 'from', subnet,
            'to', gateway, 'port', '18082', 'comment', 'PonyNotes private knowledge search'], check=True)
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
    block = marker + f'''    location = /api/knowledge/search {{
      client_max_body_size 16k;
      proxy_pass http://{gateway}:18082;
      proxy_read_timeout 120s;
    }}
'''
    if marker not in original:
        backup = nginx.with_name('nginx.conf.before-knowledge-search')
        if not backup.exists():
            backup.write_text(original)
        nginx.write_text(original.replace('    location /gotrue/ {', block + '    location /gotrue/ {', 1))
    try:
        subprocess.run(['docker', 'compose', 'exec', '-T', 'nginx', 'nginx', '-t'], cwd=sync.CLOUD_ROOT, check=True)
        subprocess.run(['docker', 'compose', 'exec', '-T', 'nginx', 'nginx', '-s', 'reload'], cwd=sync.CLOUD_ROOT, check=True)
    except Exception:
        nginx.write_text(original)
        raise


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--install', action='store_true')
    parser.add_argument('--bind', default='127.0.0.1')
    args = parser.parse_args()
    if args.install:
        install()
    else:
        Server((args.bind, 18082)).serve_forever()
