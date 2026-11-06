"""Offline regression check: python test-sync-weknora.py (no credentials/network)."""
import copy
import importlib.util
from pathlib import Path
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

    def entries(self, kb):
        return [copy.deepcopy(item) for item in self.items.get(kb, {}).values()]

    def call(self, path, method='GET', payload=None):
        self.calls.append((method, path))
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

    # A failed source traversal never reaches reconcile/pruning in the orchestration.
    original_cloud, original_provision = m.Cloud, m.provision
    source.fail = True
    source_get = source.get
    source.get = lambda path: [{'workspace_id': 'w'}] if path == 'workspace' else source_get(path)
    state = {'users': {'u': {'workspaces': {'w': {'id': second['id'], 'notes': {}}}}}}
    m.Cloud = lambda *args: source
    m.provision = lambda *args: (state['users']['u'], target)
    try:
        report = m.sync(state, persist, [{'uuid': 'u'}], 'unused')
        assert len(report['errors']) == 1 and len(target.entries(second['id'])) == 1
        source.get = lambda path: [] if path == 'workspace' else source_get(path)
        # Missing membership removes only the mapped workspace KB.
        target.call = lambda path, *args: target.items.pop(path.split('/')[-1], None)
        report = m.sync(state, persist, [{'uuid': 'u'}], 'unused')
        assert not report['errors'] and not state['users']['u']['workspaces']
    finally:
        m.Cloud, m.provision = original_cloud, original_provision
    print('PASS: traversal, pagination, large notes, lost responses, idempotency, updates, deletion, model promotion, isolation, failed scans, revoked membership')


if __name__ == '__main__':
    check()
