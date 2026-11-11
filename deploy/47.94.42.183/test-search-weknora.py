"""Offline access-control regression: python test-search-weknora.py."""
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import threading
import urllib.error
import urllib.request
import uuid
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('search_notes', Path(__file__).with_name('search-weknora.py'))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
with patch.object(m.sync.urllib.request, 'urlopen', return_value=io.BytesIO(b'[]')):
    assert m.sync.request('http://example.invalid/', 'datasource') == []
wid, own_view, private_a, private_b = (str(uuid.uuid4()) for _ in range(4))
other = str(uuid.uuid4())
channel = 'pn:' + 'a' * 40
members = {'user-a', 'user-b', 'new-member'}
workspace_owner = 1
entries = {}
hits = []
used_keys = []
wiki_enabled = True
created = {}
lost_creates = set()
pages = [
    {'slug': 'shared', 'title': 'Shared Wiki', 'source_refs': ['shared|Shared note'],
        'out_links': ['private-b', 'mixed'], 'content': 'Shared Wiki content'},
    {'slug': 'private-b', 'title': 'B Wiki', 'source_refs': ['private-b|B note'],
        'out_links': ['shared'], 'content': 'B Wiki content'},
    {'slug': 'mixed', 'title': 'Mixed private Wiki', 'source_refs': ['private-a|A', 'private-b|B'],
        'out_links': ['shared'], 'content': 'Must not leak mixed Wiki'},
    {'slug': 'manual', 'title': 'Manual Wiki', 'source_refs': [],
        'out_links': [], 'content': 'Owner-only Wiki'},
]
for page in pages:
    page['knowledge_base_id'] = 'workspace-kb'


def add(oid, text, content, kb='workspace-kb', source_channel=channel):
    entries[oid] = {'knowledge_base_id': kb, 'channel': source_channel,
        'title': oid, 'metadata': {'content': content}}
    hits.append({'knowledge_id': oid, 'content': text})


add('private-a', 'A private', f'ponynotes://open?viewId={private_a}&workspaceId={wid}')
add('private-b', 'B private', f'ponynotes://open?viewId={private_b}&workspaceId={wid}')
add('shared', 'Shared note', f'ponynotes://open?viewId={own_view}&workspaceId={wid}')
hits.append({'knowledge_id': 'shared', 'content': 'Duplicate chunk'})
add('quick-a', 'A quick', f'来源：小马笔记\n\n目录：随手记\n\n所属用户：user-a\n\nA text')
# An embedded document URL must not bypass quick-note ownership.
add('quick-b', 'B quick', f'来源：小马笔记\n\n目录：随手记\n\n所属用户：user-b\n\n'
    f'ponynotes://open?viewId={own_view}&workspaceId={wid}')
add('legacy-quick', 'Must not leak legacy quick', '来源：小马笔记\n\n目录：随手记\n\nUnscoped old note')
add('wrong-kb', 'Must not leak wrong KB', f'ponynotes://open?viewId={own_view}&workspaceId={wid}', kb='other-kb')
add('wrong-space', 'Must not leak wrong space', f'ponynotes://open?viewId={own_view}&workspaceId={other}')
add('unmanaged', 'Must not leak unmanaged', f'ponynotes://open?viewId={own_view}&workspaceId={wid}', source_channel='web')


class Client:
    def __init__(self, key):
        assert key == 'server-only-workspace-key'
        used_keys.append(key)

    def call(self, path, method='GET', payload=None):
        if path == 'knowledge-bases' and method == 'POST':
            assert payload['embedding_model_id'] == 'workspace-embedding'
            assert payload['summary_model_id'] == 'workspace-chat'
            assert payload['type'] == 'document'
            created[payload['id']] = payload
            if payload['id'] in lost_creates:
                lost_creates.remove(payload['id'])
                raise m.sync.APIError(503)  # The server created it but the response was lost.
            return {'data': payload}
        if path.startswith('knowledge-bases/'):
            bid = path.split('/')[1]
            if bid != 'workspace-kb':
                if bid not in created:
                    raise m.sync.APIError(404)
                return {'data': [] if path.endswith('/hybrid-search') else created[bid]}
        if path == 'knowledge-bases/workspace-kb':
            return {'data': {'id': 'workspace-kb', 'embedding_model_id': 'embedding',
                'type': 'document', 'indexing_strategy': {'wiki_enabled': wiki_enabled}}}
        if path.startswith('datasource?'):
            assert path == 'datasource?kb_id=workspace-kb'
            return [{'id': 'feishu', 'name': 'Team Feishu', 'type': 'feishu', 'status': 'active',
                'knowledge_base_id': kb, 'config': {'credentials': 'must-not-leak'},
                'error_message': 'must-not-leak', 'last_sync_cursor': 'must-not-leak'}
                for kb in ['workspace-kb', 'wrong-kb']]
        if path.startswith('knowledgebase/workspace-kb/wiki/pages?'):
            number = int(path.split('page=')[1].split('&')[0])
            return {'pages': pages[(number - 1) * 2:number * 2], 'total': len(pages)}
        if path.startswith('knowledgebase/workspace-kb/wiki/pages/'):
            return next(page for page in pages if page['slug'] == path.split('/')[-1])
        if path.endswith('/hybrid-search'):
            assert path == 'knowledge-bases/workspace-kb/hybrid-search'
            return {'data': hits}
        return {'data': entries[path.split('/')[-1]]}

    def entries(self, kb):
        return [{**entry, 'id': kid} for kid, entry in entries.items()]


def request(base, path, **kwargs):
    authorization = kwargs['headers']['Authorization']
    assert authorization.startswith('Bearer valid-')
    uid = authorization.removeprefix('Bearer valid-')
    if path == '/gotrue/user':
        return {'id': uid}
    if path == 'user/profile':
        return {'data': {'uuid': uid, 'uid': {'user-a': 1, 'user-b': 2, 'new-member': 3}.get(uid, 4)}}
    if path == 'workspace?include_role=true':
        return {'data': [{'workspace_id': wid, 'workspace_name': 'Team', 'owner_uid': workspace_owner,
            'role': 'Owner' if uid == 'user-a' else 'Guest' if uid == 'new-member' else 'Member'}]
            if uid in members else []}
    if '/folder?' in path:
        oid = path.split('root_view_id=')[1]
        visible = [own_view] + ([private_a] if uid == 'user-a' else [private_b] if uid == 'user-b' else [])
        return {'data': {'view_id': oid, 'children': [{'view_id': view} for view in visible] if oid == wid else []}}
    raise AssertionError(path)


with tempfile.TemporaryDirectory() as directory:
    m.sync.ROOT = Path(directory)
    (Path(directory) / 'sync').mkdir()
    path = Path(directory) / 'sync/state.json'
    state = {'service': {'api_key': 'server-only-workspace-key'},
        'workspaces': {wid: {'id': 'workspace-kb', 'ready': True}},
        'users': {'user-a': {'api_key': 'never-use-legacy-key', 'workspaces': {wid: {'id': 'legacy-kb'}}}}}
    path.write_text(json.dumps(state))
    m.sync.request, m.sync.WeKnora = request, Client
    result = m.search('Bearer valid-user-a', {'query': 'a question', 'workspace_id': wid, 'user_id': 'user-b', 'kb_id': 'kb-b'})
    assert result['status'] == 'matched'
    assert {s['content'] for s in result['sources']} == {'Shared note', 'A private', 'A quick'}
    assert 'key' not in json.dumps(result) and 'Must not leak' not in json.dumps(result)
    result = m.search('Bearer valid-user-b', {'query': 'test', 'workspace_id': wid})
    assert {s['content'] for s in result['sources']} == {'Shared note', 'B private', 'B quick'}
    result = m.search('Bearer valid-new-member', {'query': 'test', 'workspace_id': wid})
    assert [s['content'] for s in result['sources']] == ['Shared note']
    assert len(used_keys) == 3  # members do not need a personal WeKnora account
    before = path.read_text()
    a, b = 'Bearer valid-user-a', 'Bearer valid-user-b'
    assert m.library(a, 'bases', {})['bases'][0]['can_manage'] is True
    assert m.library(b, 'bases', {})['bases'][0]['can_manage'] is False
    m.library(b, 'favorite', {'workspace_id': wid, 'is_favorite': True})
    assert m.library(b, 'bases', {})['bases'][0]['is_favorite'] is True
    assert m.library(a, 'bases', {})['bases'][0]['is_favorite'] is False
    assert {d['id'] for d in m.library(b, 'documents', {'workspace_id': wid})['documents']} == {'shared', 'private-b', 'quick-b'}
    uploads = [str(uuid.uuid4()) for _ in range(3)]
    for kid, kind in zip(uploads, ('file', 'url', 'manual')):
        add(kid, kind, 'Workspace upload content', source_channel='web')
        entries[kid]['type'] = kind
    assert set(uploads) <= {d['id'] for d in m.library(b, 'documents', {'workspace_id': wid})['documents']}
    assert m.library(b, 'document', {'workspace_id': wid, 'document_id': uploads[0]})['content'] == 'Workspace upload content'
    changed_channel = str(uuid.uuid4())
    add(changed_channel, 'private quick note',
        '来源：小马笔记\n\n目录：随手记\n\n所属用户：user-a\n\nMust not leak', source_channel='web')
    entries[changed_channel]['type'] = 'manual'
    assert changed_channel not in {d['id'] for d in m.library(b, 'documents', {'workspace_id': wid})['documents']}
    sources = m.library(b, 'sources', {'workspace_id': wid})['sources']
    assert len(sources) == 1 and sources[0]['name'] == 'Team Feishu'
    assert 'must-not-leak' not in json.dumps(sources) and 'config' not in sources[0]
    wiki = m.library(b, 'wiki', {'workspace_id': wid})['pages']
    assert {page['id'] for page in wiki} == {'shared', 'private-b'}
    assert wiki[0]['out_links'] == ['private-b']
    wiki = m.library(a, 'wiki', {'workspace_id': wid})['pages']
    assert {page['id'] for page in wiki} == {'shared', 'manual'}
    assert wiki[0]['out_links'] == []  # inaccessible graph endpoints are removed
    assert m.library(b, 'wiki_page', {'workspace_id': wid, 'slug': 'shared'})['content'] == 'Shared Wiki content'
    for slug, status in [('mixed', 403), ('manual', 403), ('../private-b', 400), ('', 400), ('a\\b', 400)]:
        try:
            m.library(b, 'wiki_page', {'workspace_id': wid, 'slug': slug})
            raise AssertionError('Unreadable/invalid Wiki page allowed')
        except m.sync.APIError as error:
            assert error.status == status
    wiki_enabled = False
    assert m.library(b, 'wiki', {'workspace_id': wid})['pages'] == []
    wiki_enabled = True
    document_id = str(uuid.uuid4())
    add(document_id, 'document', f'ponynotes://open?viewId={own_view}&workspaceId={wid}')
    assert m.library(b, 'document', {'workspace_id': wid, 'document_id': document_id})['url'].endswith(wid)
    entries[document_id]['metadata']['content'] = f'ponynotes://open?viewId={private_b}&workspaceId={wid}'
    try:
        m.library(a, 'document', {'workspace_id': wid, 'document_id': document_id})
        raise AssertionError('Private page read through direct document API')
    except m.sync.APIError as error:
        assert error.status == 403
    m.library(a, 'visibility', {'workspace_id': wid, 'visibility': 'private'})
    assert m.library(b, 'bases', {})['bases'] == []  # favorite never grants access
    assert m.library(a, 'bases', {})['bases'][0]['visibility'] == 'private'
    for action, payload in [('search', {'query': 'test'}), ('documents', {}),
            ('document', {'document_id': document_id}), ('sources', {}), ('wiki', {}),
            ('wiki_page', {'slug': 'shared'}), ('favorite', {'is_favorite': True}),
            ('visibility', {'visibility': 'shared'})]:
        try:
            payload = {**payload, 'workspace_id': wid}
            m.search(b, payload) if action == 'search' else m.library(b, action, payload)
            raise AssertionError('Private KB access allowed: ' + action)
        except m.sync.APIError as error:
            assert error.status == 403
    m.library(a, 'visibility', {'workspace_id': wid, 'visibility': 'shared'})
    try:
        m.library(b, 'visibility', {'workspace_id': wid, 'visibility': 'private'})
        raise AssertionError('Member changed visibility')
    except m.sync.APIError as error:
        assert error.status == 403
    for action, extra in [('favorite', {'is_favorite': 'true'}),
            ('visibility', {'visibility': 'public'}), ('document', {'document_id': '../invalid'})]:
        try:
            m.library(a, action, {'workspace_id': wid, **extra})
            raise AssertionError('Invalid library parameters allowed')
        except m.sync.APIError as error:
            assert error.status == 400
    m.library(a, 'visibility', {'workspace_id': wid, 'visibility': 'private'})
    workspace_owner = 2
    assert m.library(a, 'bases', {})['bases'] == []
    assert m.library(b, 'bases', {})['bases'][0]['can_manage'] is True
    m.library(b, 'visibility', {'workspace_id': wid, 'visibility': 'shared'})
    workspace_owner = 1
    assert path.read_text() == before  # separate metadata cannot race the sync writer
    m.library(b, 'favorite', {'workspace_id': wid, 'is_favorite': False})
    assert m.library(b, 'bases', {})['bases'][0]['is_favorite'] is False
    state['workspaces'][wid]['embedding_proxy'] = {'id': 'workspace-embedding', 'ready': True}
    state['service']['summary_model_id'] = 'workspace-chat'
    path.write_text(json.dumps(state))
    immutable_state = path.read_text()
    create = {'workspace_id': wid, 'name': '  项目资料  ', 'visibility': 'private',
        'request_id': str(uuid.uuid4()), 'owner_uuid': 'user-b'}
    private = m.library(a, 'create', create)['base']
    assert private['name'] == '项目资料' and private['visibility'] == 'private'
    assert private['workspace_id'] == wid and private['id'] != wid
    assert private['can_manage'] and private['ready']
    assert created[private['id']]['summary_model_id'] == 'workspace-chat'
    assert m.library(a, 'create', create)['base'] == private and len(created) == 1
    assert private['id'] not in {base['id'] for base in m.library(b, 'bases', {})['bases']}
    for action, extra in [('documents', {}), ('favorite', {'is_favorite': True}),
            ('search', {'query': 'test'}), ('visibility', {'visibility': 'shared'})]:
        try:
            payload = {'workspace_id': wid, 'base_id': private['id'], **extra}
            m.search(b, payload) if action == 'search' else m.library(b, action, payload)
            raise AssertionError('New private KB leaked: ' + action)
        except m.sync.APIError as error:
            assert error.status == 403
    m.library(a, 'visibility', {'workspace_id': wid, 'base_id': private['id'], 'visibility': 'shared'})
    shared = next(base for base in m.library(b, 'bases', {})['bases'] if base['id'] == private['id'])
    assert shared['visibility'] == 'shared' and not shared['can_manage']
    m.library(b, 'favorite', {'workspace_id': wid, 'base_id': shared['id'], 'is_favorite': True})
    assert next(base for base in m.library(b, 'bases', {})['bases'] if base['id'] == shared['id'])['is_favorite']
    assert not m.library(b, 'bases', {})['bases'][0]['is_favorite']  # Defaults remain independent.
    assert m.library(a, 'documents', {'workspace_id': wid, 'base_id': shared['id']})['documents'] == []
    assert m.library(a, 'wiki', {'workspace_id': wid, 'base_id': shared['id']})['pages'] == []
    member_private = m.library(b, 'create', {**create, 'request_id': str(uuid.uuid4())})['base']
    assert member_private['id'] not in {base['id'] for base in m.library(a, 'bases', {})['bases']}
    lost = {**create, 'name': 'Recovered', 'visibility': 'shared', 'request_id': str(uuid.uuid4())}
    lost_id = str(uuid.uuid5(uuid.NAMESPACE_URL, f"ponynotes:base:user-a:{wid}:{lost['request_id']}"))
    lost_creates.add(lost_id)
    try:
        m.library(a, 'create', lost)
        raise AssertionError('Expected a lost create response')
    except m.sync.APIError as error:
        assert error.status == 503
    assert not m.settings()['bases'][lost_id]['ready']
    assert m.library(a, 'create', lost)['base']['id'] == lost_id and len(created) == 3
    for extra, authorization, status in [({'name': ''}, a, 400), ({'name': '  '}, a, 400),
            ({'name': 'x' * 101}, a, 400), ({'name': 'line\nbreak'}, a, 400),
            ({'visibility': 'public'}, a, 400), ({'request_id': 'invalid'}, a, 400),
            ({'workspace_id': other}, a, 403), ({}, 'Bearer valid-new-member', 403)]:
        try:
            m.library(authorization, 'create', {**create, 'request_id': str(uuid.uuid4()), **extra})
            raise AssertionError('Invalid/unauthorized create allowed')
        except m.sync.APIError as error:
            assert error.status == status
    assert path.read_text() == immutable_state  # Custom libraries never overwrite sync/billing mappings.
    del entries[document_id]
    hits.pop()
    server = m.Server(('127.0.0.1', 0))
    serving = threading.Thread(target=server.serve_forever, daemon=True)
    serving.start()
    try:
        endpoint = f'http://127.0.0.1:{server.server_port}/api/knowledge/'
        req = urllib.request.Request(endpoint + 'bases', data=b'{}',
            headers={'Authorization': a, 'Content-Type': 'application/json'})
        with urllib.request.urlopen(req, timeout=5) as response:
            assert response.headers['Cache-Control'] == 'no-store'
            assert json.load(response)['data']['bases'][0]['workspace_id'] == wid
        for action in ('sources', 'wiki', 'wiki_page', 'create'):
            req = urllib.request.Request(endpoint + action,
                data=json.dumps({**create, 'workspace_id': wid, 'slug': 'shared',
                    'request_id': str(uuid.uuid4())}).encode(),
                headers={'Authorization': b, 'Content-Type': 'application/json'})
            with urllib.request.urlopen(req, timeout=5) as response:
                assert json.load(response)['success'] is True
        for action, data, expected in [('bases', b'{}', 401),
                ('unknown', b'{}', 404), ('bases', b'[]', 400)]:
            req = urllib.request.Request(endpoint + action, data=data)
            try:
                urllib.request.urlopen(req, timeout=5)
                raise AssertionError('Invalid HTTP request allowed')
            except urllib.error.HTTPError as error:
                assert error.code == expected
    finally:
        server.shutdown()
        serving.join(timeout=5)
        server.server_close()
        server.pool.shutdown()
    used_keys.clear()
    members.remove('user-b')
    try:
        m.library(b, 'documents', {'workspace_id': wid, 'base_id': member_private['id']})
        raise AssertionError('Removed member retained their custom library')
    except m.sync.APIError as error:
        assert error.status == 403
    try:
        m.search('Bearer valid-user-b', {'query': 'test', 'workspace_id': wid})
        raise AssertionError('Removed member retained access')
    except m.sync.APIError as error:
        assert error.status == 403
    assert len(used_keys) == 0  # denied before any shared-tenant request
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
    state['workspaces'][wid]['ready'] = False
    path.write_text(json.dumps(state))
    assert m.search('Bearer valid-user-a', {'query': 'test', 'workspace_id': wid})['status'] == 'awaiting_sync'
    del state['workspaces']
    path.write_text(json.dumps(state))
    assert m.search('Bearer valid-user-a', {'query': 'test', 'workspace_id': wid})['status'] == 'awaiting_sync'
print('PASS: workspace/custom KB creation and retry recovery, Chinese names, guest rejection, library visibility/favorites, file/source/Wiki APIs, Wiki source permissions and graph links, credential redaction, HTTP routes, membership revocation, private pages, quick notes, isolation, validation, migration readiness')
