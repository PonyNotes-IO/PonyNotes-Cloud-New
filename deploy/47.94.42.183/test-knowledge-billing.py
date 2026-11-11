"""Offline owner/token accounting regression: python test-knowledge-billing.py."""
import base64
import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import threading
import urllib.error
import urllib.request
import uuid

spec = importlib.util.spec_from_file_location('search_bridge', Path(__file__).with_name('search-weknora.py'))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
s = m.sync
wid, other = str(uuid.uuid4()), str(uuid.uuid4())


def denied(status, call):
    try:
        call()
        raise AssertionError('Expected rejection')
    except s.APIError as error:
        assert error.status == status


def check_embeddings():
    original = s.ROOT, s.sql, s.cloud_secret, s.request
    calls, charges = [], []
    owners = [{'uuid': str(uuid.uuid4()), 'email': 'old@example.test'}]
    next_owner = {'uuid': str(uuid.uuid4()), 'email': 'new@example.test'}
    response = {'embeddings': [[0.5, 0.25]], 'prompt_eval_count': 7}
    transfer_during_inference = [False]

    def sql(query):
        calls.append('owner')
        assert f"w.workspace_id='{wid}'::uuid" in query
        assert 'w.owner_uid' in query and 'w.deleted_at IS NULL' in query
        return copy.deepcopy(owners)

    def request(base, path, method='GET', payload=None, headers=None):
        calls.append(path)
        if path == '/api/embed':
            assert base == 'http://127.0.0.1:11434'
            assert payload == {'model': 'local-model', 'input': ['text'], 'truncate': True,
                'options': {'num_ctx': 2048}, 'dimensions': 2}
            if transfer_during_inference[0]:
                owners[:] = [next_owner]
            return copy.deepcopy(response)
        assert path == '/api/subscription/usage/record' and method == 'POST'
        assert payload == {'usage_type': 'ai_tokens', 'usage_count': 7}
        encoded = headers['Authorization'].split('.')[1]
        user = json.loads(base64.urlsafe_b64decode(encoded + '=' * (-len(encoded) % 4)))['sub']
        charges.append((user, payload['usage_count']))
        return {'code': 0}

    try:
        with tempfile.TemporaryDirectory() as directory:
            s.ROOT = Path(directory)
            (s.ROOT / 'sync').mkdir()
            state = {'workspaces': {wid: {'embedding_proxy': {'token': 'workspace-secret',
                'name': 'local-model', 'parameters': {'dimension': 2, 'truncate_prompt_tokens': 2048,
                    'supports_dimension_override': True}}}}}
            s.save(s.ROOT / 'sync/state.json', state)
            s.sql, s.cloud_secret, s.request = sql, lambda: 'test-secret', request
            payload = {'model': 'local-model', 'input': 'text', 'user_id': 'spoofed-billing-user'}
            auth = 'Bearer workspace-secret'
            old_owner = owners[0]['uuid']
            transfer_during_inference[0] = True
            result = m.embedding(auth, wid, payload)
            assert result['usage'] == {'prompt_tokens': 7, 'total_tokens': 7}
            assert charges == [(old_owner, 7)]  # capture owner before inference
            m.embedding(auth, wid, payload)
            assert charges[-1] == (next_owner['uuid'], 7)  # future work follows transfer
            assert len(charges) == 2 and calls.count('/api/embed') == 2
            before = calls[:]
            denied(401, lambda: m.embedding('Bearer member-login-token', wid, payload))
            denied(401, lambda: m.embedding(auth, other, payload))
            denied(400, lambda: m.embedding(auth, wid, {**payload, 'model': 'other-model'}))
            denied(400, lambda: m.embedding(auth, wid, {**payload, 'input': [1]}))
            assert calls == before  # no model execution or owner billing for invalid requests
            owners.clear()
            denied(403, lambda: m.embedding(auth, wid, payload))
            assert calls.count('/api/embed') == 2
            owners[:] = [next_owner]
            for invalid in (None, True, -1, 2**63):
                response['prompt_eval_count'] = invalid
                denied(503, lambda: m.embedding(auth, wid, payload))
            assert len(charges) == 2  # missing usage is never estimated
            response['prompt_eval_count'] = 7
            response['embeddings'] = [[float('nan'), 0]]
            denied(503, lambda: m.embedding(auth, wid, payload))
            assert len(charges) == 3  # actual inference still consumed tokens
            response['embeddings'] = [[0.5, 0.25]]
            for internal in (False, True):
                server = m.Server(('127.0.0.1', 0), internal=internal)
                serving = threading.Thread(target=server.serve_forever, daemon=True)
                serving.start()
                try:
                    path = f'/internal/knowledge/{wid}/v1/embeddings'
                    req = urllib.request.Request(f'http://127.0.0.1:{server.server_port}' + path,
                        data=json.dumps(payload).encode(), headers={'Authorization': auth})
                    try:
                        with urllib.request.urlopen(req, timeout=5) as http:
                            assert internal and json.load(http)['data'][0]['embedding'] == [0.5, 0.25]
                    except urllib.error.HTTPError as error:
                        assert not internal and error.code == 404
                finally:
                    server.shutdown()
                    serving.join(timeout=5)
                    server.server_close()
                    server.pool.shutdown()
    finally:
        s.ROOT, s.sql, s.cloud_secret, s.request = original


def check_model_recovery():
    original = s.ROOT, s.WeKnora
    models, persisted = [], []
    source = {'name': 'local-model', 'type': 'Embedding', 'source': 'local',
        'parameters': {'embedding_parameters': {'dimension': 2, 'truncate_prompt_tokens': 2048}}}
    state, mapping = {'service': {'tenant_id': 1}}, {}
    lose_response = [True]

    class Client:
        def __init__(self, token=None):
            self.token = token

        def call(self, path, method='GET', payload=None):
            if path == 'auth/login':
                return {'active_tenant': {'id': 1}, 'token': 'admin'}
            assert self.token == 'admin'
            if path == 'models/source':
                return {'data': source}
            assert path == 'models'
            if method == 'POST':
                assert persisted[-1]['embedding_proxy']['token'] == payload['parameters']['api_key']
                assert payload['parameters']['embedding_parameters'] == source['parameters']['embedding_parameters']
                models.append({**copy.deepcopy(payload), 'id': str(uuid.uuid4())})
                if lose_response[0]:
                    lose_response[0] = False
                    raise TimeoutError('Created, response lost')
                return {'data': models[-1]}
            return {'data': models}

    try:
        with tempfile.TemporaryDirectory() as directory:
            s.ROOT, s.WeKnora = Path(directory), Client
            (s.ROOT / 'library').mkdir()
            (s.ROOT / 'library/runtime.json').write_text(json.dumps(
                {'embedding_url': 'http://172.20.0.1:18083/internal/knowledge'}))
            (s.ROOT / 'admin-credentials.json').write_text('{}')
            persist = lambda: persisted.append(copy.deepcopy(mapping))
            try:
                s.workspace_embedding(state, wid, mapping, persist, 'source')
                raise AssertionError('Timeout swallowed')
            except TimeoutError:
                pass
            assert not mapping['embedding_proxy']['ready'] and len(models) == 1
            token = mapping['embedding_proxy']['token']
            kid = s.workspace_embedding(state, wid, mapping, persist, 'source')
            assert kid == models[0]['id'] and mapping['embedding_proxy']['ready']
            assert mapping['embedding_proxy']['token'] == token and len(models) == 1
            assert s.workspace_embedding(state, wid, mapping, persist, kid) == kid
            assert s.workspace_embedding(state, other, {}, lambda: None, '') == ''
    finally:
        s.ROOT, s.WeKnora = original


if __name__ == '__main__':
    check_embeddings()
    check_model_recovery()
    print('PASS: live owner and transfer, actual tokens only, zero extra AI questions, '
        'secret/workspace/model isolation, failed usage, private HTTP route, lost model response recovery')
