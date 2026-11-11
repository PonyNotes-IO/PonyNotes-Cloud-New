#!/usr/bin/env bash
set -euo pipefail
root=/opt/ponynotes
export PATH="/root/.cargo/bin:$PATH"
cd "$root"
cloud_only=false
if [[ "${2:-}" == --cloud-only ]]; then cloud_only=true; fi

for tool in docker python3 tar rsync cargo protoc flock; do
  command -v "$tool" >/dev/null || { echo "Missing server tool: $tool" >&2; exit 1; }
done
if ! "$cloud_only"; then command -v go >/dev/null; fi
for file in .env compose.yaml config/cloud.env config/gotrue.env source/.cargo/config.toml; do
  test -f "$file" || { echo "Missing existing test-server configuration: $file" >&2; exit 1; }
done
docker compose config --quiet
if [[ -f compose.override.yaml ]] && ! head -1 compose.override.yaml | grep -qx '# Managed by deploy-test.py'; then
  echo 'compose.override.yaml is not managed by this script; inspect it before deploying.' >&2
  exit 1
fi
echo 'PASS test-server prerequisites and existing Compose configuration'
if [[ "${1:-}" == --check ]]; then exit 0; fi

release="${1:?Missing release ID}"
[[ "$release" =~ ^test-[0-9]{8}-[0-9]{6}-[0-9a-f]{8}$ ]] || exit 1
stage="$root/releases/$release"
test -d "$stage/tools"
exec 9>"$root/.deploy.lock"
flock -n 9 || { echo 'Another deployment is running.' >&2; exit 1; }
exec > >(tee -a "$stage/deploy.log") 2>&1
export PONYNOTES_DEPLOY_TAG="$release"

backup="$root/backups/$release"
mkdir -p "$backup"
chmod 700 "$backup" "$stage"
cp -a .env compose.yaml config "$backup/"
for file in compose.override.yaml ruoyi-admin.jar deployment.json; do
  if [[ -f "$file" ]]; then cp -a "$file" "$backup/"; fi
done
echo "Backup: $backup"
trap 'status=$?; if ((status)); then echo "FAILED: log $stage/deploy.log; backup $backup" >&2; fi' EXIT

# Cache dependencies in Docker; the local Windows machine needs no JDK or Maven.
maven_image=maven:3.9.9-eclipse-temurin-17
if ! "$cloud_only"; then
  docker image inspect "$maven_image" >/dev/null 2>&1 || docker pull "$maven_image"
fi

# Retain server-only dependency mirrors and build caches; remove deleted source files.
rsync -a --checksum --delete --exclude target --exclude .cargo "$stage/cloud/" "$root/source/"
if ! "$cloud_only"; then
  rsync -a --checksum --delete --exclude auth "$stage/gotrue/" "$root/gotrue-source/"
  mkdir -p "$root/web-source"
  rsync -a --checksum --delete --exclude target "$stage/web/" "$root/web-source/"
fi
for file in build-cloud.sh build-gotrue.sh Dockerfile.cloud Dockerfile.gotrue .dockerignore migrate-cloud.py smoke.py test-otp-smoke.py; do
  cp "$stage/tools/$file" "$root/$file"
done

echo '[1/4] Build backend images'
if ! "$cloud_only"; then
  (cd gotrue-source && GOPROXY=https://goproxy.cn GOMAXPROCS=2 CGO_ENABLED=0 go test -mod=readonly -p 2 ./internal/conf -run '^TestMailerTestOTP$' -count=1)
  bash build-gotrue.sh
fi
bash build-cloud.sh
if ! "$cloud_only"; then
  echo '[2/4] Build Web-Desktop Java backend'
  docker run --rm -v "$root/web-source:/workspace" -v ponynotes_maven_cache:/root/.m2 -w /workspace \
    "$maven_image" mvn -B -ntp -pl ruoyi-admin -am -DskipTests clean package
  test -s web-source/ruoyi-admin/target/ruoyi-admin.jar
fi

echo '[3/4] Back up PostgreSQL and apply Cloud migrations'
docker compose exec -T postgres pg_dump -U postgres -d ponynotes -Fc > "$backup/postgres.dump"
python3 migrate-cloud.py

python3 - "$release" "$cloud_only" <<'PY'
import json, subprocess, sys
from pathlib import Path
release, cloud_only = sys.argv[1:]
override = Path('compose.override.yaml')
data = json.loads(subprocess.check_output(['docker', 'compose', '-f', str(override),
    'config', '--format', 'json', '--no-interpolate'], text=True)) if override.exists() else {}
services = data.setdefault('services', {})
for name in ['appflowy_cloud', 'appflowy_worker']:
    services.setdefault(name, {})['image'] = 'ponynotes/cloud:' + release
if cloud_only != 'true':
    services.setdefault('gotrue', {})['image'] = 'ponynotes/gotrue:' + release
Path('compose.override.yaml.next').write_text('# Managed by deploy-test.py\n' + json.dumps(data, indent=2) + '\n')
PY
mv compose.override.yaml.next compose.override.yaml
docker compose config --quiet
if ! "$cloud_only"; then
  cp web-source/ruoyi-admin/target/ruoyi-admin.jar ruoyi-admin.jar.next
  mv ruoyi-admin.jar.next ruoyi-admin.jar
fi

echo '[4/4] Update services and check real authentication/API requests'
if ! "$cloud_only"; then
  docker compose up -d --no-deps --pull never --wait --wait-timeout 120 gotrue
fi
docker compose up -d --no-deps --pull never --wait --wait-timeout 300 appflowy_cloud
if "$cloud_only"; then
  docker compose up -d --no-deps --pull never --wait --wait-timeout 300 appflowy_worker
else
  docker compose up -d --no-deps --pull never --force-recreate --wait --wait-timeout 300 appflowy_worker web_backend
fi
python3 smoke.py
# Run the dedicated OTP check only when these test accounts are configured.
if grep -q 'otp-smoke@ponynotes.test:123456' config/gotrue.env && grep -q '19900000002:123456' config/gotrue.env; then
  python3 test-otp-smoke.py
fi
python3 - "$stage" <<'PY'
from datetime import datetime, timezone
import hashlib, json, sys
from pathlib import Path
import subprocess
stage = Path(sys.argv[1])
path = Path('deployment.json')
data = json.loads(path.read_text()) if path.exists() else {}
manifest = json.loads((stage / 'manifest.json').read_text())
data['latest_deployment'] = manifest | {'release': stage.name, 'validated_at': datetime.now(timezone.utc).isoformat(), 'log': str(stage / 'deploy.log')}
for source, key in [('cloud','cloud_source_commit'),('gotrue','gotrue_source_commit'),('web','web_source_commit')]:
    if source in manifest['sources']:
        data[key] = manifest['sources'][source]['commit']
for service, image in [('gotrue','gotrue'),('appflowy_cloud','cloud')]:
    if image == 'gotrue' and 'gotrue' not in manifest['sources']:
        continue
    name = 'ponynotes/' + image + ':' + stage.name
    data.setdefault('images', {})[service] = {'name': name, 'id': subprocess.check_output(['docker','image','inspect',name,'--format','{{.Id}}'],text=True).strip()}
if 'web' in manifest['sources']:
    data['java_jar_sha256'] = hashlib.sha256(Path('ruoyi-admin.jar').read_bytes()).hexdigest()
if 'gotrue' in manifest['sources']:
    data['gotrue_binary_sha256'] = hashlib.sha256(Path('gotrue-source/auth').read_bytes()).hexdigest()
data['migration_count'] = int(subprocess.check_output(['docker','compose','exec','-T','postgres','psql','-U','postgres','-d','ponynotes','-Atc','SELECT count(*) FROM _sqlx_migrations WHERE success'],text=True).strip())
data['validated_at'] = data['latest_deployment']['validated_at']
path.write_text(json.dumps(data, indent=2, ensure_ascii=False) + '\n')
PY
echo "PASS deployment $release; log: $stage/deploy.log"
