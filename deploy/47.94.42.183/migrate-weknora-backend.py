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
    shutil.copy2(LEGACY / 'sync/state.json', backup / 'legacy-state.json')
    shutil.copy2(LEGACY / 'compose.local.json', backup / 'weknora-compose.local.json')
    active = {u: subprocess.run(['systemctl', 'is-active', '--quiet', u]).returncode == 0 for u in UNITS}
    enabled = {u: subprocess.run(['systemctl', 'is-enabled', '--quiet', u], capture_output=True).returncode == 0 for u in UNITS}
    atomic(backup / 'units.json', json.dumps({'active': active, 'enabled': enabled}))
    print('BACKUP', backup, flush=True)
    try:
        # Stop the legacy writer before taking the authoritative state snapshot.
        run('systemctl', 'stop', UNITS[0], UNITS[1])
        state = json.loads((LEGACY / 'sync/state.json').read_text())
        assert isinstance(state['users'], dict)
        shutil.copy2(LEGACY / 'sync/state.json', backup / 'legacy-state.json')
        migration = ROOT / 'source/migrations/20261106020000_weknora_sync_state.sql'
        checksum = hashlib.sha384(migration.read_bytes()).hexdigest()
        old = sql("SELECT encode(checksum,'hex') FROM _sqlx_migrations WHERE version=20261106020000;").strip()
        assert not old or old == checksum, 'Applied migration checksum differs'
        if not old:
            sql("BEGIN;\n" + migration.read_text() + "\nINSERT INTO _sqlx_migrations(version,description,success,checksum,execution_time) VALUES (20261106020000,'weknora sync state',true,decode('" + checksum + "','hex'),0);\nCOMMIT;")
        # Existing native state is never overwritten on repeat deployment.
        serialized = json.dumps(state, ensure_ascii=False).replace("'", "''")
        sql("INSERT INTO af_weknora_sync_state(singleton,state) VALUES(true,'" + serialized + "'::jsonb) ON CONFLICT(singleton) DO NOTHING;")
        sql('UPDATE af_weknora_sync_state SET last_run=NULL WHERE singleton;')
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
        env_path = ROOT / 'config/cloud.env'
        env = env_path.read_text()
        values = {'PONYNOTES_WEKNORA_ENABLED': 'true', 'PONYNOTES_WEKNORA_URL': 'http://ponynotes-weknora-api:8080/api/v1/', 'PONYNOTES_WEKNORA_CLOUD_URL': 'http://nginx', 'PONYNOTES_WEKNORA_ADMIN_FILE': '/run/weknora-admin.json'}
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
        run('docker', 'compose', 'up', '-d', '--no-deps', '--no-build', '--pull', 'never', '--wait', '--wait-timeout', '300', 'appflowy_worker')
        # Cloud is healthy; route the existing URL to its native authenticated handler.
        nginx = (ROOT / 'nginx.conf').read_text()
        nginx = re.sub(r'    # PonyNotes knowledge search\n    location = /api/knowledge/search \{.*?\n    \}\n', '', nginx, flags=re.S)
        assert ':18082' not in nginx, 'Unknown legacy search route; inspect manually'
        # Nginx bind-mounts this file: preserve its inode when changing the route.
        (ROOT / 'nginx.conf').write_text(nginx)
        run('docker', 'compose', 'exec', '-T', 'nginx', 'nginx', '-t')
        run('docker', 'compose', 'exec', '-T', 'nginx', 'nginx', '-s', 'reload')
        for _ in range(60):
            report = sql("SELECT coalesce(last_run,'null'::jsonb) FROM af_weknora_sync_state WHERE singleton;").strip()
            if report and report != 'null':
                report = json.loads(report)
                assert not report['errors'], 'Native sync reported errors'
                assert report['workspaces'], 'Native sync covered no workspaces'
                break
            time.sleep(2)
        else:
            raise RuntimeError('No native sync report')
        for unit in UNITS:
            run('systemctl', 'disable', '--now', unit)
        atomic(backup / 'result.json', json.dumps({'image': image, 'report': report, 'backup': str(backup)}, ensure_ascii=False, indent=2))
        print('PASS native Cloud retrieval and Worker sync; legacy units disabled', flush=True)
        print(json.dumps({'workspaces': len(report['workspaces']), 'errors': len(report['errors'])}), flush=True)
    except BaseException:
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
