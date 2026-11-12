"""Run on the test server after building the Cloud/Worker image: --image IMAGE.

One-time deployment tool; synchronization and retrieval run entirely in Rust.
"""
import argparse
from datetime import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import time
import urllib.request
import urllib.error

import yaml  # Already installed on the test server.

ROOT = Path('/opt/ponynotes')
LEGACY = Path('/opt/ponynotes-weknora')
UNITS = ['ponynotes-weknora-sync.timer', 'ponynotes-weknora-sync.service', 'ponynotes-weknora-search.service']
FILES = ['compose.yaml', 'compose.override.yaml', 'nginx.conf', 'config/cloud.env']


def run(*command, data=None):
    result = subprocess.run(command, cwd=ROOT, input=data, text=True, capture_output=True)
    if result.returncode:
        # Never print a Compose config, SQL payload, credentials or private notes.
        raise RuntimeError('Command failed: ' + command[0])
    return result.stdout


def sql(query):
    return run('docker', 'compose', 'exec', '-T', 'postgres', 'psql', '-U', 'postgres', '-d', 'ponynotes', '-At', '-v', 'ON_ERROR_STOP=1', data=query)


def update_weknora_app():
    result = subprocess.run(['docker', 'compose', '-p', 'ponynotes-weknora', '-f', 'compose.yaml', '-f', 'compose.local.json', '-f', 'images.lock.json', 'up', '-d', '--no-build', '--pull', 'never', '--wait', '--wait-timeout', '300', 'app'], cwd=LEGACY, capture_output=True)
    if result.returncode:
        raise RuntimeError('WeKnora app network update failed')


def atomic(path, content):
    temp = path.with_suffix(path.suffix + '.next')
    temp.write_text(content)
    temp.chmod(0o600)
    os.replace(temp, path)


def merge_state(current, legacy, settings):
    """Import once; retain pending native IDs/secrets if a prior cutover failed."""
    current = current or {'state': {'users': {}, 'workspaces': {}}, 'settings': {}}
    state = current['state']
    if state.get('native_library_version') == 1:
        return state, current.get('settings') or {}
    state = {**state, **{k: v for k, v in legacy.items() if k not in ('users', 'workspaces')}}
    state['users'] = {**current['state'].get('users', {}), **legacy.get('users', {})}
    mappings = dict(current['state'].get('workspaces', {}))
    for wid, mapping in legacy.get('workspaces', {}).items():
        mappings[wid] = {**mappings.get(wid, {}), **mapping}
    state['workspaces'] = mappings
    state['native_library_version'] = 0  # Mark complete only after native sync and routing pass.
    previous = current.get('settings') or {}
    config = {**previous, **settings}
    for key in ('bases', 'workspaces', 'favorites'):
        config[key] = {**previous.get(key, {}), **settings.get(key, {})}
    return state, config


def native_nginx(config):
    config = re.sub(r'    # PonyNotes knowledge search\n    location [^\n]+\{\n.*?^    }\n', '', config, flags=re.M | re.S)
    assert ':18082' not in config and ':18083' not in config, 'Unrecognized legacy route'
    if 'location ^~ /internal/knowledge/' not in config:
        marker = '    location /gotrue/ {'
        assert marker in config, 'Nginx insertion point missing'
        config = config.replace(marker, '    location ^~ /internal/knowledge/ { return 404; }\n' + marker, 1)
    return config


def weknora_admin(path, method='GET', payload=None, token=None):
    headers = {'Content-Type': 'application/json'}
    if token:
        headers['Authorization'] = 'Bearer ' + token
    request = urllib.request.Request('http://127.0.0.1:18081/api/v1/' + path, method=method,
        data=json.dumps(payload).encode() if payload is not None else None, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=90) as response:
            result = json.load(response)
            assert result.get('success') is not False
            return result
    except Exception:
        raise RuntimeError('WeKnora administrative request failed') from None


def apply_migration(version, name):
    migration = ROOT / 'source/migrations' / name
    checksum = hashlib.sha384(migration.read_bytes()).hexdigest()
    old = sql(f"SELECT encode(checksum,'hex') FROM _sqlx_migrations WHERE version={version};").strip()
    assert not old or old == checksum, 'Applied migration checksum differs'
    if not old:
        sql("BEGIN;\n" + migration.read_text() + f"\nINSERT INTO _sqlx_migrations(version,description,success,checksum,execution_time) VALUES ({version},'weknora native library',true,decode('{checksum}','hex'),0);\nCOMMIT;")


def probe_native():
    container = run('docker', 'compose', 'ps', '-q', 'appflowy_cloud').strip()
    network = json.loads(run('docker', 'inspect', container))[0]['NetworkSettings']['Networks']['ponynotes-knowledge']
    for path in ('/api/knowledge/web_import_options', '/internal/knowledge/00000000-0000-0000-0000-000000000000/v1/embeddings'):
        request = urllib.request.Request('http://' + network['IPAddress'] + ':8000' + path,
            data=b'{}', headers={'Content-Type': 'application/json'})
        try:
            urllib.request.urlopen(request, timeout=15).close()
        except urllib.error.HTTPError as error:
            if error.code == 401:
                continue
        raise RuntimeError('Image does not provide the authenticated native knowledge routes')


def migrate(image):
    os.umask(0o077)
    run('docker', 'image', 'inspect', image)
    run('docker', 'compose', 'config', '--quiet')
    backup = ROOT / 'backups' / ('weknora-native-' + datetime.now().strftime('%Y%m%d-%H%M%S'))
    backup.mkdir(mode=0o700)
    for name in FILES:
        dest = backup / name
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / name, dest)
    if (LEGACY / 'sync/state.json').exists():
        shutil.copy2(LEGACY / 'sync/state.json', backup / 'legacy-state.json')
    shutil.copy2(LEGACY / 'compose.local.json', backup / 'weknora-compose.local.json')
    active = {u: subprocess.run(['systemctl', 'is-active', '--quiet', u]).returncode == 0 for u in UNITS}
    enabled = {u: subprocess.run(['systemctl', 'is-enabled', '--quiet', u], capture_output=True).returncode == 0 for u in UNITS}
    atomic(backup / 'units.json', json.dumps({'active': active, 'enabled': enabled}))
    print('BACKUP', backup, flush=True)
    model_backup = []
    admin_token = None
    try:
        # Freeze both old writers, including settings mutations, before importing.
        for unit in UNITS:
            if subprocess.run(['systemctl', 'cat', unit], capture_output=True).returncode == 0:
                run('systemctl', 'stop', unit)
        run('docker', 'compose', 'stop', 'appflowy_worker', 'appflowy_cloud')
        legacy_path = LEGACY / 'sync/state.json'
        legacy = json.loads(legacy_path.read_text()) if legacy_path.exists() else {'users': {}, 'workspaces': {}}
        settings_path = LEGACY / 'library/settings.json'
        settings = json.loads(settings_path.read_text()) if settings_path.exists() else {}
        assert isinstance(legacy.get('users', {}), dict) and isinstance(legacy.get('workspaces', {}), dict)
        assert isinstance(settings, dict)
        atomic(backup / 'legacy-state.json', json.dumps(legacy))
        atomic(backup / 'legacy-settings.json', json.dumps(settings))
        existed = sql("SELECT to_regclass('public.af_weknora_sync_state') IS NOT NULL;").strip() == 't'
        previous = sql('SELECT row_to_json(t) FROM af_weknora_sync_state t WHERE singleton;').strip() if existed else ''
        atomic(backup / 'native-before.json', previous or 'null')
        apply_migration(20261106020000, '20261106020000_weknora_sync_state.sql')
        apply_migration(20261112010000, '20261112010000_weknora_library_settings.sql')
        state, config = merge_state(json.loads(previous) if previous else None, legacy, settings)
        state_json = json.dumps(state, ensure_ascii=False).replace("'", "''")
        config_json = json.dumps(config, ensure_ascii=False).replace("'", "''")
        sql("UPDATE af_weknora_sync_state SET state='" + state_json + "'::jsonb,settings='" + config_json + "'::jsonb,last_run=NULL,updated_at=now() WHERE singleton;")
        credentials = json.loads((LEGACY / 'admin-credentials.json').read_text())
        admin_token = weknora_admin('auth/login', 'POST', credentials)['token']
        model_backup = [model for model in weknora_admin('models', token=admin_token)['data'] or []
            if model.get('description', '').startswith('PonyNotes workspace embedding ')]
        atomic(backup / 'embedding-models.json', json.dumps(model_backup))
        if subprocess.run(['docker', 'network', 'inspect', 'ponynotes-knowledge'], capture_output=True).returncode:
            run('docker', 'network', 'create', 'ponynotes-knowledge')
        # A dedicated bridge avoids collisions between the two stacks' postgres/redis DNS aliases.
        weknora_path = LEGACY / 'compose.local.json'
        weknora = json.loads(weknora_path.read_text())
        weknora.setdefault('networks', {})['ponynotes-knowledge'] = {'external': True, 'name': 'ponynotes-knowledge'}
        app = weknora.setdefault('services', {}).setdefault('app', {})
        app_networks = app.get('networks', {'WeKnora-network': None})
        if isinstance(app_networks, list):
            app_networks = {name: None for name in app_networks}
        app_networks['ponynotes-knowledge'] = {'aliases': ['ponynotes-weknora-api']}
        app['networks'] = app_networks
        app_env = app.setdefault('environment', {})
        whitelist = [v for v in app_env.get('SSRF_WHITELIST_EXTRA', '').split(',') if v]
        if 'appflowy_cloud' not in whitelist:
            whitelist.append('appflowy_cloud')
        app_env['SSRF_WHITELIST_EXTRA'] = ','.join(whitelist)
        atomic(weknora_path, json.dumps(weknora, indent=2) + '\n')
        update_weknora_app()
        config = yaml.safe_load((ROOT / 'compose.yaml').read_text())
        config.setdefault('networks', {})['knowledge'] = {'external': True, 'name': 'ponynotes-knowledge'}
        for name in ['appflowy_cloud', 'appflowy_worker']:
            service = config['services'][name]
            networks = service.get('networks', ['default'])
            if isinstance(networks, list):
                service['networks'] = list(dict.fromkeys(networks + ['knowledge']))
            else:
                networks['knowledge'] = None
                service['networks'] = networks
        worker = config['services']['appflowy_worker']
        volumes = worker.setdefault('volumes', [])
        mount = '/opt/ponynotes-weknora/admin-credentials.json:/run/weknora-admin.json:ro'
        if mount not in volumes:
            volumes.append(mount)
        atomic(ROOT / 'compose.yaml', yaml.safe_dump(config, sort_keys=False))
        network = json.loads(run('docker', 'network', 'inspect', 'ponynotes_default'))[0]
        gateway = network['IPAM']['Config'][0]['Gateway']
        if shutil.which('ufw'):
            bridge = network.get('Options', {}).get('com.docker.network.bridge.name') or 'br-' + network['Id'][:12]
            run('ufw', 'allow', 'in', 'on', bridge, 'proto', 'tcp', 'from',
                network['IPAM']['Config'][0]['Subnet'], 'to', gateway, 'port', '11434',
                'comment', 'PonyNotes native embedding accounting')
        env_path = ROOT / 'config/cloud.env'
        env = env_path.read_text()
        values = {'PONYNOTES_WEKNORA_ENABLED': 'true', 'PONYNOTES_WEKNORA_URL': 'http://ponynotes-weknora-api:8080/api/v1/', 'PONYNOTES_WEKNORA_CLOUD_URL': 'http://nginx', 'PONYNOTES_WEKNORA_ADMIN_FILE': '/run/weknora-admin.json', 'PONYNOTES_WEKNORA_EMBEDDING_URL': 'http://appflowy_cloud:8000/internal/knowledge', 'PONYNOTES_WEKNORA_OLLAMA_URL': f'http://{gateway}:11434'}
        for key, value in values.items():
            env = re.sub(r'(?m)^' + key + r'=.*\n?', '', env)
            env = env.rstrip('\n') + '\n' + key + '=' + value + '\n'
        atomic(env_path, env)
        override_path = ROOT / 'compose.override.yaml'
        override = yaml.safe_load(override_path.read_text())
        for name in ['appflowy_cloud', 'appflowy_worker']:
            override['services'].setdefault(name, {})['image'] = image
        atomic(override_path, '# Managed by deploy-test.py\n' + yaml.safe_dump(override, sort_keys=False))
        run('docker', 'compose', 'config', '--quiet')
        run('docker', 'compose', 'up', '-d', '--no-deps', '--no-build', '--pull', 'never', '--wait', '--wait-timeout', '300', 'appflowy_cloud')
        probe_native()
        run('docker', 'compose', 'up', '-d', '--no-deps', '--no-build', '--pull', 'never', '--wait', '--wait-timeout', '300', 'appflowy_worker')
        # Native Worker first moves existing embedding callbacks; keep public reads paused.
        for _ in range(900):
            report = sql("SELECT coalesce(last_run,'null'::jsonb) FROM af_weknora_sync_state WHERE singleton;").strip()
            if report and report != 'null':
                report = json.loads(report)
                assert not report['errors'], 'Native sync reported errors'
                assert report['workspaces'], 'Native sync covered no workspaces'
                break
            time.sleep(2)
        else:
            raise RuntimeError('No native sync report')
        nginx = native_nginx((ROOT / 'nginx.conf').read_text())
        # Preserve inode for the Nginx bind mount.
        (ROOT / 'nginx.conf').write_text(nginx)
        run('docker', 'compose', 'exec', '-T', 'nginx', 'nginx', '-t')
        run('docker', 'compose', 'exec', '-T', 'nginx', 'nginx', '-s', 'reload')
        for unit in UNITS:
            if subprocess.run(['systemctl', 'cat', unit], capture_output=True).returncode == 0:
                run('systemctl', 'disable', '--now', unit)
        atomic(backup / 'result.json', json.dumps({'image': image, 'report': report, 'backup': str(backup)}, ensure_ascii=False, indent=2))
        sql("UPDATE af_weknora_sync_state SET state=jsonb_set(state,'{native_library_version}','1'::jsonb) WHERE singleton;")
        print('PASS native knowledge management, retrieval, billing and sync; legacy units disabled', flush=True)
        print(json.dumps({'workspaces': len(report['workspaces']), 'errors': len(report['errors'])}), flush=True)
    except BaseException:
        run('docker', 'compose', 'stop', 'appflowy_worker', 'appflowy_cloud')
        # Restore callback URLs with the same model IDs; PUT preserves stored credentials.
        if admin_token:
            for model in model_backup:
                weknora_admin('models/' + model['id'], 'PUT', {key: model.get(key) for key in
                    ('name', 'type', 'source', 'description', 'parameters')}, token=admin_token)
        # Keep pending native IDs/tokens in SQL for retry recovery. Old processes use their untouched files.
        for name in FILES:
            shutil.copy2(backup / name, ROOT / name)
        shutil.copy2(backup / 'weknora-compose.local.json', LEGACY / 'compose.local.json')
        update_weknora_app()
        run('docker', 'compose', 'up', '-d', '--no-deps', '--no-build', '--pull', 'never', '--wait', '--wait-timeout', '300', 'appflowy_cloud', 'appflowy_worker')
        run('docker', 'compose', 'exec', '-T', 'nginx', 'nginx', '-t')
        run('docker', 'compose', 'exec', '-T', 'nginx', 'nginx', '-s', 'reload')
        for unit in UNITS:
            if enabled[unit]:
                run('systemctl', 'enable', unit)
            if active[unit] and not unit.endswith('sync.service'):
                run('systemctl', 'start', unit)
        print('Restored prior service configuration; backup:', backup, flush=True)
        raise


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--image', required=True)
    args = parser.parse_args()
    migrate(args.image)
