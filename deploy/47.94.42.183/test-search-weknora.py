"""Offline access-control regression: python test-search-weknora.py."""
import importlib.util
import json
from pathlib import Path
import tempfile
import uuid

spec = importlib.util.spec_from_file_location('search_notes', Path(__file__).with_name('search-weknora.py'))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
wid, own_view, private_view = (str(uuid.uuid4()) for _ in range(3))
other = str(uuid.uuid4())
channel = 'pn:' + 'a' * 40


class Client:
    def __init__(self, key):
        assert key == 'only-user-a-key'

    def call(self, path, method='GET', payload=None):
        if path == 'knowledge-bases/kb-a':
            return {'data': {'id': 'kb-a', 'embedding_model_id': 'embedding'}}
        if path.endswith('/hybrid-search'):
            return {'data': [{'knowledge_id': 'private', 'content': 'Must not leak'},
                {'knowledge_id': 'own', 'content': 'Visible note'},
                {'knowledge_id': 'own', 'content': 'Duplicate chunk'}]}
        oid = own_view if path == 'knowledge/own' else private_view
        return {'data': {'knowledge_base_id': 'kb-a', 'channel': channel,
            'title': 'note', 'metadata': {'content': f'ponynotes://open?viewId={oid}&workspaceId={wid}'}}}


def request(base, path, **kwargs):
    assert kwargs['headers']['Authorization'] == 'Bearer valid-user-a'
    if path == '/gotrue/user':
        return {'id': 'user-a'}
    if path == 'workspace':
        return {'data': [{'workspace_id': wid}]}
    if '/folder?' in path:
        oid = path.split('root_view_id=')[1]
        return {'data': {'view_id': oid, 'children': [{'view_id': own_view}] if oid == wid else []}}
    raise AssertionError(path)


with tempfile.TemporaryDirectory() as directory:
    m.sync.ROOT = Path(directory)
    (Path(directory) / 'sync').mkdir()
    path = Path(directory) / 'sync/state.json'
    path.write_text(json.dumps({'users': {'user-a': {'api_key': 'only-user-a-key',
        'workspaces': {wid: {'id': 'kb-a'}}}, 'user-b': {'api_key': 'never-use',
        'workspaces': {other: {'id': 'kb-b'}}}}}))
    m.sync.request, m.sync.WeKnora = request, Client
    result = m.search('Bearer valid-user-a', {'query': 'a question', 'workspace_id': wid, 'user_id': 'user-b', 'kb_id': 'kb-b'})
    assert result['status'] == 'matched'
    assert len(result['sources']) == 1 and result['sources'][0]['content'] == 'Visible note'
    assert 'key' not in json.dumps(result) and 'Must not leak' not in json.dumps(result)
    try:
        m.search('Bearer valid-user-a', {'query': 'test', 'workspace_id': other})
        raise AssertionError('Cross-workspace read allowed')
    except m.sync.APIError as error:
        assert error.status == 403
    for auth, payload in [('', {'query': 'test', 'workspace_id': wid}),
        ('Bearer valid-user-a', {'query': '', 'workspace_id': wid}),
        ('Bearer valid-user-a', {'query': 'test', 'workspace_id': '../invalid'})]:
        try:
            m.search(auth, payload)
            raise AssertionError('Invalid input accepted')
        except m.sync.APIError as error:
            assert error.status in (400, 401)
    path.write_text('{"users":{}}')
    assert m.search('Bearer valid-user-a', {'query': 'test', 'workspace_id': wid})['status'] == 'awaiting_sync'
print('PASS: authenticated identity, current page visibility, tenant isolation, source deduplication, validation, new-user sync delay')
