"""Offline cutover checks: python test-migrate-weknora-backend.py."""
import copy
from pathlib import Path
import runpy

m = runpy.run_path(str(Path(__file__).with_name('migrate-weknora-backend.py')))
legacy = {'users': {'old': {}}, 'service': {'api_key': 'private'},
          'workspaces': {'wid': {'id': 'existing-kb', 'embedding_proxy': {'token': 'existing-token'}}}}
settings = {'bases': {'custom': {'id': 'custom', 'name': '神', 'owner_uuid': 'owner', 'visibility': 'private', 'ready': True}},
            'favorites': {'owner': ['custom']}}
current = {'state': {'users': {'native-old': {}}, 'workspaces': {'pending': {'id': 'pending-kb', 'embedding_proxy': {'token': 'pending-secret'}}}}, 'settings': {}}
state, config = m['merge_state'](copy.deepcopy(current), copy.deepcopy(legacy), copy.deepcopy(settings))
assert state['workspaces']['wid'] == legacy['workspaces']['wid']
assert state['workspaces']['pending'] == current['state']['workspaces']['pending']
assert state['users'] == {'old': {}, 'native-old': {}}
assert state['native_library_version'] == 0 and config == {**settings, 'workspaces': {}}
state['native_library_version'] = 1
state2, config2 = m['merge_state']({'state': state, 'settings': config}, {}, {'bases': {}})
assert state2 == state and config2 == config  # Repeat deployments never reset native settings.
for location in ('= /api/knowledge/search', '^~ /api/knowledge/'):
    original = ('server {\n    # PonyNotes knowledge search\n    location ' + location + ' {\n'
                '      proxy_pass http://172.20.0.1:18082;\n    }\n'
                '    location /gotrue/ {\n      proxy_pass http://gotrue;\n    }\n}\n')
    updated = m['native_nginx'](original)
    assert '18082' not in updated and 'location ^~ /internal/knowledge/ { return 404; }' in updated
    assert 'proxy_pass http://gotrue;' in updated
    assert m['native_nginx'](updated) == updated
try:
    m['native_nginx']('server { location /custom { proxy_pass http://host:18082; } }')
    raise AssertionError('Unexpected route was silently removed')
except AssertionError as error:
    assert str(error) == 'Unrecognized legacy route'
print('PASS legacy/custom IDs, permission settings, pending recovery, repeat deployment and Nginx cutover')
