"""Offline regression check: python test-sync-weknora.py (no credentials/network)."""
import copy
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import urllib.parse

spec = importlib.util.spec_from_file_location('bridge', Path(__file__).with_name('sync-weknora.py'))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


def document():
    return {'page_id': 'root', 'blocks': {
        'root': {'ty': 'page', 'children': 'ordered'},
        'first': {'ty': 'heading', 'external_id': 't1', 'data': '{"level":2}'},
        'second': {'ty': 'paragraph', 'external_id': 't2'},
        'deleted': {'ty': 'paragraph', 'external_id': 'secret'}},
        'meta': {'children_map': {'ordered': ['first', 'second']},
            'text_map': {'t1': '标题', 't2': '正文', 'secret': 'Deleted text must stay deleted'}}}


class Target:
    def __init__(self):
        self.items, self.calls, self.lose_create = {}, [], False
        self.kbs, self.lose_kb_create = {}, False

    def entries(self, kb):
        return [copy.deepcopy(item) for item in self.items.get(kb, {}).values()]

    def call(self, path, method='GET', payload=None):
        self.calls.append((method, path))
        if path == 'knowledge-bases' and method == 'POST':
            self.kbs[payload['id']] = copy.deepcopy(payload)
            if self.lose_kb_create:
                self.lose_kb_create = False
                raise TimeoutError('KB created, response lost')
            return {'data': self.kbs[payload['id']]}
        if path.startswith('knowledge-bases/') and len(path.split('/')) == 2:
            kb = path.split('/')[1]
            if method == 'DELETE':
                self.kbs.pop(kb)
                self.items.pop(kb, None)
                return {'success': True}
            if kb not in self.kbs:
                raise m.APIError(404)
            if method == 'PUT':
                self.kbs[kb].update(copy.deepcopy(payload))
            return {'data': self.kbs[kb]}
        if method == 'POST':
            kb = path.split('/')[1]
            oid = str(len(self.calls))
            item = {'id': oid, 'channel': payload['channel']}
            self.items.setdefault(kb, {})[oid] = item
        else:
            oid = path.split('/')[-1]
            item = next(i[oid] for i in self.items.values() if oid in i)
            if method == 'DELETE':
                for items in self.items.values():
                    items.pop(oid, None)
                return {'success': True}
        item.update(title=payload['title'], metadata={'content': payload['content'], 'status': payload['status']},
            parse_status='draft' if payload['status'] == 'draft' else 'pending')
        if self.lose_create and method == 'POST':
            self.lose_create = False
            raise TimeoutError('Create succeeded, HTTP response lost')
        return {'data': copy.deepcopy(item)}


class Source(m.Cloud):
    def __init__(self):
        self.fail = False
        self.user = {'uuid': 'u'}

    def get(self, path):
        url = urllib.parse.urlparse(path)
        query = urllib.parse.parse_qs(url.query)
        if url.path.endswith('/folder'):
            oid = query['root_view_id'][0]
            index = 0 if oid == 'w' else int(oid)
            node = {'view_id': oid, 'name': oid, 'is_space': index < 14, 'layout': 0,
                'children': [{'view_id': str(index + 1)}] if index < 14 else []}
            return node
        if '/collab/' in path:
            if self.fail:
                raise TimeoutError()
            return {'collab': {'document': document()}}
        if url.path.endswith('/database'):
            return []
        if url.path.endswith('/quick-note'):
            start = int(query['offset'][0])
            return {'has_more': start == 0, 'quick_notes': [
                {'id': 'q' + str(i), 'created_at': '2026-11-06',
                    'data': [{'type': 'paragraph', 'attributes': {'delta': [{'insert': 'Quick ' + str(i)}]}}]}
                for i in range(start, min(101, start + 100))]}
        raise AssertionError(path)


def check():
    assert m.document_markdown(document()) == '## 标题\n\n正文'
    assert m.quick_markdown({'type': 'paragraph', 'data': {'delta': [{'insert': '正文'}]}}) == '正文\n'
    bad = document()
    bad['meta']['children_map']['ordered'].append('root')
    try:
        m.document_markdown(bad)
        raise AssertionError('Cycle accepted')
    except ValueError:
        pass
    long_text = '笔记' * 210000
    parts = m.note_parts('w', 'document', 'doc', 'Title', long_text)
    assert len(parts) == 3 and all(len(p['content']) <= 200000 for p in parts.values())
    header = next(iter(m.note_parts('w', 'document', 'doc', 'Title', '').values()))['content'].removesuffix('（空白笔记）')
    assert ''.join(p['content'][len(header):] for p in parts.values()) == long_text
    private_anchor = m.note_parts('w', 'row', 'row-id', 'Row', 'Body', view_id='private-view')
    shared_anchor = m.note_parts('w', 'row', 'row-id', 'Row', 'Body', view_id='shared-view')
    assert set(private_anchor).isdisjoint(shared_anchor)
    source = Source()
    notes, counts = source.scan('w')
    assert counts == {'documents': 1, 'quick_notes': 101} and len(notes) == 102
    assert any('Quick 100' in p['content'] for p in notes.values())

    target, cache = Target(), {}
    kb = {'id': 'tenant-a-kb'}
    desired = m.note_parts('w', 'document', 'doc', 'Title', 'Body')
    channel = next(iter(desired))
    persist = lambda: None
    target.lose_create = True
    try:
        m.reconcile(target, kb, desired, cache, persist)
        raise AssertionError('Timeout was swallowed')
    except TimeoutError:
        pass
    m.reconcile(target, kb, desired, cache, persist)
    assert len(target.entries(kb['id'])) == 1  # recovered by channel after lost create response
    count = len(target.calls)
    result = m.reconcile(target, kb, desired, cache, persist)
    assert result['unchanged'] == 1 and len(target.calls) == count
    desired[channel].update(title='Renamed', content='Changed body')
    assert m.reconcile(target, kb, desired, cache, persist)['updated'] == 1
    assert len(target.entries(kb['id'])) == 1
    kb['embedding_model_id'] = 'configured-model'
    assert m.reconcile(target, kb, desired, cache, persist)['pending'] == 1
    second = {'id': 'tenant-b-kb'}
    m.reconcile(target, second, desired, {}, persist)
    assert len(target.entries(second['id'])) == 1
    assert m.reconcile(target, kb, {}, cache, persist)['deleted'] == 1
    assert len(target.entries(second['id'])) == 1  # tenant B is unaffected

    check_workspaces()
    check_provision()
    check_migration_backup()
    print('PASS: document extraction, paging, large notes, lost responses, idempotency, workspace aggregation, migration, membership, failed scans, deletion, service credentials')


def check_workspaces():
    original_cloud, original_provision = m.Cloud, m.provision
    original_embedding = m.workspace_embedding
    target = Target()
    # Existing per-user KBs and uploaded files must survive the migration.
    state = {'users': {'a': {'api_key': 'legacy-a', 'workspaces': {'w': {'id': 'old-a'}}},
        'b': {'api_key': 'legacy-b', 'workspaces': {'w': {'id': 'old-b'}}}}}
    legacy = copy.deepcopy(state['users'])
    users = [{'uuid': 'a'}, {'uuid': 'b'}]
    workspaces = [{'workspace_id': 'w', 'owner_uuid': 'a', 'workspace_name': 'Team', 'members': ['a', 'b']},
        {'workspace_id': 'other', 'owner_uuid': 'a', 'workspace_name': 'Other', 'members': ['a']}]
    failures, scans = set(), []

    class Member:
        def __init__(self, user, secret):
            self.uid = user['uuid']

        def scan(self, wid):
            scans.append((wid, self.uid))
            if (wid, self.uid) in failures:
                raise TimeoutError()
            notes = m.note_parts(wid, 'document', 'shared', 'Shared', 'Team content')
            notes.update(m.note_parts(wid, 'document', self.uid, 'Private', self.uid + ' only'))
            notes.update(m.note_parts(wid, 'quick', self.uid, 'Quick', self.uid + ' quick',
                '随手记', owner_id=self.uid))
            return notes, {'documents': 2, 'quick_notes': 1}

    def provision(state, persist):
        state['service'] = {'api_key': 'server-only', 'embedding_model_id': 'embedding'}
        return state['service'], target

    persist = lambda: None
    m.Cloud, m.provision = Member, provision
    m.workspace_embedding = lambda state, wid, mapping, persist, source_id: source_id
    try:
        report = m.sync(state, persist, users, workspaces, 'unused', scan_only=True)
        assert not report['errors'] and state == {'users': legacy} and not target.calls
        target.lose_kb_create = True
        report = m.sync(state, persist, users, workspaces, 'unused')
        assert len(report['errors']) == 1 and not state['workspaces']['w']['ready']
        report = m.sync(state, persist, users, workspaces, 'unused')
        assert not report['errors'] and len(target.kbs) == 2
        mapping = state['workspaces']['w']
        kid = mapping['id']
        assert mapping['ready'] and kid != 'old-a' and kid != 'old-b'
        assert len(target.entries(kid)) == 5  # one shared doc + two private docs + two quick notes
        assert state['users'] == legacy
        assert target.kbs[kid]['embedding_model_id'] == 'embedding'
        assert any('所属用户：b' in item['metadata']['content'] for item in target.entries(kid))

        # Drafts can receive their first model; populated indexed bases cannot silently switch.
        target.kbs[kid]['embedding_model_id'] = ''
        for item in target.items[kid].values():
            item['metadata']['status'], item['parse_status'] = 'draft', 'draft'
        report = m.sync(state, persist, users, workspaces, 'unused')
        assert not report['errors'] and target.kbs[kid]['embedding_model_id'] == 'embedding'
        assert all(item['metadata']['status'] == 'publish' for item in target.items[kid].values())
        m.workspace_embedding = lambda state, wid, mapping, persist, source_id: 'metered' if wid == 'w' else source_id
        before = copy.deepcopy(target.items[kid])
        report = m.sync(state, persist, users, workspaces, 'unused')
        assert len(report['errors']) == 1 and target.items[kid] == before and not mapping['ready']
        assert target.kbs[kid]['embedding_model_id'] == 'embedding'
        m.workspace_embedding = lambda state, wid, mapping, persist, source_id: source_id

        target.items[kid]['upload'] = {'id': 'upload', 'channel': 'web', 'parse_status': 'completed'}
        calls = len(target.calls)
        report = m.sync(state, persist, users, list(reversed(workspaces)), 'unused')
        assert not report['errors'] and len(target.kbs) == 2
        assert all(method == 'GET' for method, _ in target.calls[calls:])

        # A failed scan for any member preserves the entire shared index.
        failures.add(('w', 'b'))
        before = copy.deepcopy(target.items[kid])
        report = m.sync(state, persist, users, workspaces, 'unused')
        assert len(report['errors']) == 1 and target.items[kid] == before
        failures.clear()

        # Ownership/name changes and a member leaving keep the same KB.
        workspaces[0].update(owner_uuid='b', workspace_name='Renamed', members=['b'])
        report = m.sync(state, persist, [{'uuid': 'b'}], workspaces[:1], 'unused')
        assert not report['errors'] and state['workspaces']['w']['id'] == kid
        assert len(target.kbs) == 1 and set(state['workspaces']) == {'w'}
        assert len(target.entries(kid)) == 4 and 'upload' in target.items[kid]
        assert scans[-1] == ('w', 'b')
        assert state['users'] == legacy

        workspaces[0]['members'] = []
        before = copy.deepcopy(target.items[kid])
        report = m.sync(state, persist, [], workspaces[:1], 'unused')
        assert len(report['errors']) == 1 and target.items[kid] == before
        report = m.sync(state, persist, [], [], 'unused')
        assert not report['errors'] and not target.kbs and not state['workspaces']
    finally:
        m.Cloud, m.provision = original_cloud, original_provision
        m.workspace_embedding = original_embedding


def check_provision():
    original_client, original_root = m.WeKnora, m.ROOT
    calls, keys = [], []
    models = [{'id': 'embedding', 'type': 'Embedding'}, {'id': 'chat', 'type': 'KnowledgeQA'},
        {'id': 'workspace-binding', 'type': 'Embedding', 'description': 'PonyNotes workspace embedding w'}]

    class Client:
        def __init__(self, token=None, key=None):
            self.headers = {'token': token, 'key': key}

        def call(self, path, method='GET', payload=None):
            calls.append((method, path))
            if path == 'auth/login':
                assert payload == {'email': 'server-admin', 'password': 'server-secret'}
                return {'token': 'admin-token', 'active_tenant': {'id': 10000}}
            assert self.headers['token'] == 'admin-token'
            if path == 'models':
                return {'data': models}
            if path == 'tenants/10000/api-keys/1' and method == 'PUT':
                assert payload['full_access'] is False
                keys[0].update(payload)
                return {'data': {k: v for k, v in keys[0].items() if k != 'token'}}
            assert path == 'tenants/10000/api-keys'
            if method == 'POST':
                assert payload['full_access'] is False
                assert set(payload['capabilities']) == {'retrieve', 'ingest', 'manage_kbs', 'manage_datasources'}
                keys.append({**payload, 'id': 1, 'token': 'workspace-key'})
                return {'data': keys[-1]}
            return {'data': keys}

    try:
        with tempfile.TemporaryDirectory() as directory:
            m.ROOT, m.WeKnora = Path(directory), Client
            (m.ROOT / 'admin-credentials.json').write_text(json.dumps(
                {'email': 'server-admin', 'password': 'server-secret'}))
            state = {}
            record, client = m.provision(state, lambda: None)
            assert record['embedding_model_id'] == 'embedding' and client.headers['key'] == 'workspace-key'
            m.provision({}, lambda: None)  # recover a key created before a lost state write
            assert len(keys) == 1
            keys[0]['capabilities'].remove('manage_datasources')
            m.provision(state, lambda: None)  # upgrade the existing scoped key in place
            assert len(keys) == 1 and 'manage_datasources' in keys[0]['capabilities']
            assert state['service']['api_key'] == 'workspace-key'
            models.append({'id': 'second', 'type': 'Embedding'})
            assert m.provision(state, lambda: None)[0]['embedding_model_id'] == ''
            models[-1]['is_default'] = True
            assert m.provision(state, lambda: None)[0]['embedding_model_id'] == 'second'
            assert all('users/create' not in path for _, path in calls)
            try:
                m.provision({'service': {'tenant_id': 999}}, lambda: None)
                raise AssertionError('Changed service tenant accepted')
            except ValueError:
                pass
    finally:
        m.WeKnora, m.ROOT = original_client, original_root


def check_migration_backup():
    original = m.ROOT, m.cloud_users, m.cloud_workspaces, m.subprocess.check_output, sys.argv[:]
    original_fcntl = sys.modules.get('fcntl')
    sys.modules['fcntl'] = SimpleNamespace(LOCK_EX=1, LOCK_NB=4, flock=lambda *args: None)
    try:
        with tempfile.TemporaryDirectory() as directory:
            m.ROOT = Path(directory)
            folder = m.ROOT / 'sync'
            folder.mkdir()
            path = folder / 'state.json'
            legacy = {'users': {'a': {'api_key': 'legacy-secret', 'workspaces': {'w': {'id': 'old-kb'}}}}}
            path.write_text(json.dumps(legacy))
            m.cloud_users, m.cloud_workspaces = lambda: [], lambda: []
            m.subprocess.check_output = lambda *args: json.dumps([
                {'Config': {'Env': ['GOTRUE_JWT_SECRET=test-only']}}]).encode()
            sys.argv = ['sync-weknora.py', '--scan-only']
            m.main()
            backup = folder / 'state.before-workspace-kbs.json'
            assert json.loads(path.read_text()) == legacy and not backup.exists()
            sys.argv = ['sync-weknora.py']
            m.main()
            original_backup = backup.read_bytes()
            assert json.loads(original_backup) == legacy
            assert json.loads(path.read_text()) == {**legacy, 'workspaces': {}}
            m.main()
            assert backup.read_bytes() == original_backup
    finally:
        m.ROOT, m.cloud_users, m.cloud_workspaces, m.subprocess.check_output, sys.argv = original
        if original_fcntl is None:
            del sys.modules['fcntl']
        else:
            sys.modules['fcntl'] = original_fcntl


if __name__ == '__main__':
    check()
