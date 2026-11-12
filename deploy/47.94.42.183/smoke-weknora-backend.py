"""Test-server checks for native retrieval and real note sync; cleans its temporary note."""
import base64
import hashlib
import hmac
import json
from pathlib import Path
import subprocess
import time
import urllib.error
import urllib.request
import uuid

ROOT = Path('/opt/ponynotes')


def command(*args):
    result = subprocess.run(args, cwd=ROOT, text=True, capture_output=True)
    if result.returncode:
        raise RuntimeError('Check command failed: ' + args[0])
    return result.stdout


def http(base, path, method='GET', payload=None, headers=None):
    request = urllib.request.Request(base + path, method=method,
        data=json.dumps(payload).encode() if payload is not None else None,
        headers={'Content-Type': 'application/json', **(headers or {})})
    with urllib.request.urlopen(request, timeout=120) as response:
        result = json.load(response)
        assert result.get('success') is not False and result.get('code', 0) == 0
        return result


def token(uid, secret):
    encode = lambda data: base64.urlsafe_b64encode(data).rstrip(b'=')
    now = int(time.time())
    body = b'.'.join([encode(b'{"alg":"HS256","typ":"JWT"}'), encode(json.dumps({
        'sub': uid, 'aud': 'authenticated', 'exp': now + 600, 'iat': now,
        'email': '', 'phone': '', 'role': 'authenticated', 'app_metadata': {}, 'user_metadata': {}}).encode())])
    return (body + b'.' + encode(hmac.new(secret.encode(), body, hashlib.sha256).digest())).decode()


def entries(key, kb):
    items, page = [], 1
    while True:
        result = http('http://127.0.0.1:18081/api/v1/', f'knowledge-bases/{kb}/knowledge?page={page}&page_size=100', headers={'X-API-Key': key})
        batch = result['data'] or []
        items.extend(batch)
        if len(items) == result['total']:
            return items
        assert batch and len(items) < result['total']
        page += 1


def sync_once():
    for _ in range(20):
        result = json.loads(command('docker', 'compose', 'exec', '-T', 'appflowy_worker', 'appflowy_worker', '--weknora-sync-once').strip().splitlines()[-1])
        assert not result['errors'], 'Native sync errors'
        if 'skipped' not in result:
            return result
        time.sleep(3)
    raise RuntimeError('Background sync did not finish')


def main():
    state = json.loads(command('docker', 'compose', 'exec', '-T', 'postgres', 'psql', '-U', 'postgres', '-d', 'ponynotes', '-Atc', 'SELECT state FROM af_weknora_sync_state WHERE singleton;'))
    container = json.loads(command('docker', 'inspect', 'ponynotes-appflowy_cloud-1'))[0]
    secret = next(v.split('=', 1)[1] for v in container['Config']['Env'] if v.startswith('APPFLOWY_GOTRUE_JWT_SECRET='))
    assert state.get('native_library_version') == 1, 'Native cutover not completed'
    config = json.loads(command('docker', 'compose', 'exec', '-T', 'postgres', 'psql', '-U', 'postgres', '-d', 'ponynotes', '-Atc', 'SELECT settings FROM af_weknora_sync_state WHERE singleton;'))
    users = json.loads(command('docker', 'compose', 'exec', '-T', 'postgres', 'psql', '-U', 'postgres', '-d', 'ponynotes', '-Atc',
        "SELECT coalesce(json_agg(u.uuid),'[]') FROM af_user u JOIN auth.users a ON a.id=u.uuid WHERE a.deleted_at IS NULL AND (a.banned_until IS NULL OR a.banned_until<now());"))
    accounts = []
    for uid in users:
        auth = {'Authorization': 'Bearer ' + token(uid, secret)}
        visible = http('http://127.0.0.1', '/api/knowledge/bases', 'POST', {}, auth)['data']['bases']
        defaults = {base['workspace_id']: state['workspaces'][base['workspace_id']]
            for base in visible if base['id'] == base['workspace_id'] and base.get('ready')}
        if defaults:
            accounts.append((uid, {'api_key': state['service']['api_key'], 'workspaces': defaults}))
        roles = {w['workspace_id']: w['role'] for w in http('http://127.0.0.1/api/', 'workspace?include_role=true', headers=auth)['data']}
        for base in visible:
            if base.get('ready') and roles.get(base['workspace_id']) in ('Owner', 'Member'):
                options = http('http://127.0.0.1', '/api/knowledge/web_import_options', 'POST',
                    {'workspace_id': base['workspace_id'], 'base_id': base['id']}, auth)
                assert set(options['data']) == {'chunk_size', 'chunk_overlap', 'strategy'}
    before_custom = {bid: {item['id'] for item in entries(state['service']['api_key'], bid)}
        for bid, base in config.get('bases', {}).items() if base.get('ready')}
    assert accounts
    before_ids = {(uid, wid): {e['id'] for e in entries(account['api_key'], mapping['id']) if e['parse_status'] != 'deleting'}
        for uid, account in accounts for wid, mapping in account['workspaces'].items()}
    checked = 0
    for uid, account in accounts:
        auth = {'Authorization': 'Bearer ' + token(uid, secret)}
        allowed = {w['workspace_id'] for w in http('http://127.0.0.1/api/', 'workspace', headers=auth)['data']}
        for wid, mapping in account['workspaces'].items():
            completed = [e for e in entries(account['api_key'], mapping['id']) if e['parse_status'] == 'completed']
            assert completed, 'No indexed note available for retrieval check'
            result = http('http://127.0.0.1', '/api/knowledge/search', 'POST', {
                'workspace_id': wid, 'query': completed[0]['title'], 'user_id': 'forged-user', 'kb_id': 'forged-kb'}, auth)['data']
            assert result['status'] == 'matched' and result['sources']
            assert set(result) == {'status', 'sources'}
            assert all(set(source) == {'title', 'url', 'content'} for source in result['sources'])
            checked += 1
        foreign = next((w for _, a in accounts for w in a['workspaces'] if w not in allowed), None)
        if foreign:
            try:
                http('http://127.0.0.1', '/api/knowledge/search', 'POST', {'workspace_id': foreign, 'query': 'test'}, auth)
                raise AssertionError('Cross-workspace retrieval allowed')
            except urllib.error.HTTPError as error:
                assert error.code == 403
    try:
        http('http://127.0.0.1', '/api/knowledge/search', 'POST', {'workspace_id': str(uuid.uuid4()), 'query': 'test'})
        raise AssertionError('Anonymous retrieval allowed')
    except urllib.error.HTTPError as error:
        assert error.code == 401
    print('PASS authenticated retrieval, forged mapping rejection and tenant isolation:', checked, flush=True)

    writable = []
    for uid, account in accounts:
        workspaces = http('http://127.0.0.1/api/', 'workspace?include_role=true', headers={'Authorization': 'Bearer ' + token(uid, secret)})['data']
        for workspace in workspaces:
            wid = workspace['workspace_id']
            if wid in account['workspaces'] and workspace['role'] in ('Owner', 'Member'):
                writable.append((uid, account, wid, account['workspaces'][wid]))
    assert writable, 'No writable workspace available'
    uid, account, wid, mapping = writable[0]
    auth = {'Authorization': 'Bearer ' + token(uid, secret)}
    marker = 'native-sync-check-' + uuid.uuid4().hex
    path = f'/api/workspace/{wid}/quick-note'
    data = lambda text: [{'type': 'paragraph', 'attributes': {'delta': [{'insert': text}]}}]
    note = http('http://127.0.0.1', path, 'POST', {'data': data(marker)}, auth)['data']
    oid = note['id']
    channel = 'pn:' + hashlib.sha256(f'{wid}/quick/{oid}/0'.encode()).hexdigest()[:40]
    find = lambda: [e for e in entries(account['api_key'], mapping['id']) if e.get('channel') == channel and e['parse_status'] != 'deleting']
    try:
        sync_once()
        found = find()
        assert len(found) == 1 and marker in found[0]['metadata']['content']
        kid = found[0]['id']
        http('http://127.0.0.1', path + '/' + oid, 'PUT', {'data': data(marker + '-updated')}, auth)
        sync_once()
        found = find()
        assert len(found) == 1 and found[0]['id'] == kid and marker + '-updated' in found[0]['metadata']['content']
        sync_once()
        assert len(find()) == 1 and find()[0]['id'] == kid
    finally:
        http('http://127.0.0.1', path + '/' + oid, 'DELETE', headers=auth)
        sync_once()
    # WeKnora accepts deletion before its background cleanup removes the entry.
    for _ in range(60):
        if not find():
            break
        time.sleep(1)
    assert not find(), 'Temporary knowledge was not removed'
    after_ids = {(uid, wid): {e['id'] for e in entries(a['api_key'], m['id']) if e['parse_status'] != 'deleting'}
        for uid, a in accounts for wid, m in a['workspaces'].items()}
    assert after_ids == before_ids, 'Preexisting knowledge IDs changed'
    after_custom = {bid: {item['id'] for item in entries(state['service']['api_key'], bid)} for bid in before_custom}
    assert before_custom == after_custom, 'Custom knowledge IDs changed'
    print('PASS native create/update/delete, idempotency, and preservation of existing knowledge IDs', flush=True)
    for unit in ['ponynotes-weknora-sync.timer', 'ponynotes-weknora-sync.service', 'ponynotes-weknora-search.service']:
        assert subprocess.run(['systemctl', 'is-active', '--quiet', unit]).returncode != 0
    print('PASS all legacy Python runtime units inactive', flush=True)


if __name__ == '__main__':
    main()
