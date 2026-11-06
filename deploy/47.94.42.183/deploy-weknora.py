"""Deploy pinned WeKnora core services independently on the existing test server."""
import argparse
import getpass
import hashlib
import io
import json
import os
from pathlib import Path
import runpy
import secrets
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request

VERSION = "v0.9.0"
COMMIT = "f17ee8a8da901bd658a4cb25f40b5fd41adbfdd7"
REMOTE = "/opt/ponynotes-weknora"
CORE = ["frontend", "app", "postgres", "redis", "docreader"]
LOCAL_OVERRIDE = {"services": {"redis": {"volumes": ["redis-data:/data"]}}, "volumes": {"redis-data": {}}}
UPSTREAM = {
    "docker-compose.yml": ("compose.yaml", "715d5e3f0e9daca1b3d8ceec7fcd7c63f711a97773df33b9c9a35d681794c4e4"),
    "config/config.yaml": ("config/config.yaml", "a748d14035c31464f1861f2cfc110f4a057832a1ef0159352d8b155dbb1f9fd5"),
}
ADMIN_EMAIL = "admin@weknora.ponynotes.test"


def new_env():
    return {
        "WEKNORA_VERSION": VERSION,
        "FRONTEND_PORT": "127.0.0.1:18080",
        "APP_PORT": "127.0.0.1:18081",
        "APP_BACKEND_PORT": "8080",
        "DB_DRIVER": "postgres", "DB_HOST": "postgres", "DB_PORT": "5432",
        "DB_USER": "weknora", "DB_NAME": "weknora", "DB_PASSWORD": secrets.token_hex(24),
        "REDIS_ADDR": "redis:6379", "REDIS_PASSWORD": secrets.token_hex(24),
        "REDIS_DB": "0", "REDIS_PREFIX": "weknora:",
        "JWT_SECRET": secrets.token_hex(32), "SYSTEM_AES_KEY": secrets.token_hex(16),
        "SYSTEM_SIGNING_KEY": secrets.token_hex(32),
        "STREAM_MANAGER_TYPE": "redis", "RETRIEVE_DRIVER": "postgres",
        "STORAGE_TYPE": "local", "LOCAL_STORAGE_BASE_DIR": "/data/files",
        "DOCREADER_ADDR": "docreader:50051", "DOCREADER_TRANSPORT": "grpc",
        "DOCREADER_GRPC_MAX_WORKERS": "2", "CONCURRENCY_POOL_SIZE": "2",
        "LOG_LEVEL": "info", "GIN_MODE": "release", "TZ": "Asia/Shanghai",
        "DEFAULT_LOCALE": "zh-CN", "OLLAMA_OPTIONAL": "true",
        "LANGFUSE_ENABLED": "false", "WEKNORA_SANDBOX_DOCKER_ENABLED": "false",
        "WEKNORA_AUTH_COMPLEX_PASSWORD_ENABLED": "true",
        "DISABLE_REGISTRATION": "false",
        "WEKNORA_BOOTSTRAP_SYSTEM_ADMIN_EMAIL": ADMIN_EMAIL,
    }


def private_write(path, text):
    # Exclusive creation preserves existing passwords and encryption keys on retry.
    with open(path, "x", encoding="utf-8", opener=lambda p, f: os.open(p, f, 0o600)) as stream:
        stream.write(text)


def upstream_files():
    files = {}
    for source, (target, expected) in UPSTREAM.items():
        url = f"https://raw.githubusercontent.com/Tencent/WeKnora/{COMMIT}/{source}"
        with urllib.request.urlopen(url, timeout=60) as response:
            content = response.read()
        if hashlib.sha256(content).hexdigest() != expected:
            raise RuntimeError(f"Upstream checksum mismatch: {source}")
        files[target] = content
    return files


def api(path, payload=None, token=None):
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = "Bearer " + token
    request = urllib.request.Request(
        "http://127.0.0.1:18081/api/v1/" + path,
        data=json.dumps(payload).encode() if payload is not None else None,
        headers=headers,
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response)


def compose(*args, capture=False):
    command = ["docker", "compose", "-p", "ponynotes-weknora", "-f", "compose.yaml"]
    command += ["-f", "compose.local.json"]
    if Path("images.lock.json").exists():
        command += ["-f", "images.lock.json"]
    return subprocess.run(command + list(args), check=True, text=True, capture_output=capture)


def on_server():
    os.chdir(REMOTE)
    for _, (target, expected) in UPSTREAM.items():
        if hashlib.sha256(Path(target).read_bytes()).hexdigest() != expected:
            raise RuntimeError(f"Uploaded checksum mismatch: {target}")
    env_file = Path(".env")
    if not Path("compose.local.json").exists():
        Path("compose.local.json").write_text(json.dumps(LOCAL_OVERRIDE, indent=2) + "\n")
    if not env_file.exists():
        private_write(env_file, "".join(f"{k}={v}\n" for k, v in new_env().items()))
    # The upstream README omits the v prefix used by the published release images.
    env_text = env_file.read_text()
    if env_text.startswith("WEKNORA_VERSION=0.9.0\n"):
        env_file.write_text(env_text.replace("WEKNORA_VERSION=0.9.0\n", f"WEKNORA_VERSION={VERSION}\n", 1))
    compose("config", "--quiet")
    if not Path("images.lock.json").exists():
        print("Pulling the five pinned core images...", flush=True)
        compose("pull", "--policy", "missing", "--quiet", *CORE)
        config = json.loads(compose("config", "--format", "json", capture=True).stdout)
        locked = {}
        for service in CORE:
            image = config["services"][service]["image"]
            metadata = json.loads(subprocess.check_output(["docker", "image", "inspect", image]))[0]
            digest = next((d for d in metadata.get("RepoDigests", []) if "@sha256:" in d), None)
            if digest is None:
                raise RuntimeError(f"Image digest missing: {service}")
            locked[service] = {"image": digest}
        Path("images.lock.json").write_text(json.dumps({"services": locked}, indent=2) + "\n")
    print("Starting isolated WeKnora services...", flush=True)
    compose("up", "-d", "--no-build", "--wait", "--wait-timeout", "600", *CORE)
    credentials_file = Path("admin-credentials.json")
    if not credentials_file.exists():
        private_write(credentials_file, json.dumps({
            "email": ADMIN_EMAIL, "username": "PonyNotesAdmin",
            "password": "Wk!" + secrets.token_urlsafe(18) + "7aA",
        }, indent=2) + "\n")
    credentials = json.loads(credentials_file.read_text())
    try:
        login = api("auth/login", credentials)
    except urllib.error.HTTPError as error:
        if error.code != 401:
            raise
        result = api("auth/register", credentials)
        if not result.get("success"):
            raise RuntimeError("Administrator registration did not succeed")
        login = api("auth/login", credentials)
    if not login.get("success") or not login.get("token"):
        raise RuntimeError("Administrator login did not succeed")
    env = env_file.read_text()
    if "DISABLE_REGISTRATION=false" in env or not login.get("user", {}).get("is_system_admin"):
        env_file.write_text(env.replace("DISABLE_REGISTRATION=false", "DISABLE_REGISTRATION=true"))
        compose("up", "-d", "--no-build", "--wait", "--wait-timeout", "600", "app", "frontend")
    login = api("auth/login", credentials)
    if not login.get("user", {}).get("is_system_admin"):
        raise RuntimeError("Administrator bootstrap was not applied")
    connectors = api("datasource/types", token=login["token"])
    serialized = json.dumps(connectors)
    if '"feishu"' not in serialized or '"feishu_drive"' not in serialized:
        raise RuntimeError("Expected Feishu connectors were not found")
    with urllib.request.urlopen("http://127.0.0.1:18080/", timeout=20) as response:
        if response.status != 200 or b"<html" not in response.read().lower():
            raise RuntimeError("Frontend is not serving HTML")
    auth_config = api("auth/config")
    if auth_config.get("registration_mode") != "invite_only" or not auth_config.get("complex_password_enabled"):
        raise RuntimeError("Expected registration and password policy was not applied")
    Path("deployment.json").write_text(json.dumps({
        "version": VERSION, "source_commit": COMMIT,
        "frontend": "http://127.0.0.1:18080", "backend": "http://127.0.0.1:18081",
        "administrator": ADMIN_EMAIL, "credentials_file": str(credentials_file.resolve()),
        "checks": ["core services healthy", "administrator login", "system admin bootstrap", "frontend HTML", "Feishu connector types"],
        "auth_config": auth_config,
        "models_configured": False,
    }, indent=2) + "\n")
    compose("ps")
    print("Verified: UI, administrator login, system admin and Feishu connector types.", flush=True)
    print(f"Administrator credentials: {credentials_file.resolve()}", flush=True)


def self_test():
    files = upstream_files()
    env = new_env()
    assert len(env["SYSTEM_AES_KEY"].encode()) == 32
    assert env["DB_PASSWORD"] != new_env()["DB_PASSWORD"]
    assert env["FRONTEND_PORT"].startswith("127.0.0.1:")
    with tempfile.TemporaryDirectory(prefix="weknora-config-") as directory:
        root = Path(directory)
        for name, content in files.items():
            target = root / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(content)
        private_write(root / ".env", "".join(f"{k}={v}\n" for k, v in env.items()))
        (root / "compose.local.json").write_text(json.dumps(LOCAL_OVERRIDE))
        try:
            private_write(root / ".env", "must not replace existing keys")
        except FileExistsError:
            pass
        else:
            raise AssertionError("Existing credentials were overwritten")
        result = subprocess.run(["docker", "compose", "-p", "ponynotes-weknora", "-f", str(root / "compose.yaml"), "-f", str(root / "compose.local.json"), "config", "--format", "json"], check=True, capture_output=True, text=True)
        config = json.loads(result.stdout)
        assert config["services"]["redis"]["volumes"][0]["source"] == "redis-data"
        for service in CORE:
            for port in config["services"][service].get("ports", []):
                assert port["host_ip"] == "127.0.0.1", service
        for service in ["app", "frontend", "docreader"]:
            assert config["services"][service]["image"].endswith(":" + VERSION)
    print("Checks passed: pinned upstream, Compose config, loopback ports and credential preservation.")


def serve_tunnel(client):
    import select
    import socketserver

    class Forwarder(socketserver.BaseRequestHandler):
        def handle(self):
            with client.get_transport().open_channel("direct-tcpip", ("127.0.0.1", 18080), self.client_address) as channel:
                while True:
                    readable, _, _ = select.select([self.request, channel], [], [], 30)
                    for source in readable:
                        data = source.recv(65536)
                        if not data:
                            return
                        (channel if source is self.request else self.request).sendall(data)

    class TunnelServer(socketserver.ThreadingTCPServer):
        allow_reuse_address = True
        daemon_threads = True

    with TunnelServer(("127.0.0.1", 18080), Forwarder) as server:
        print("SSH tunnel ready: http://127.0.0.1:18080 (keep this terminal open)", flush=True)
        server.serve_forever(poll_interval=0.5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--key", type=Path)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--tunnel", action="store_true", help="Keep an SSH tunnel open at http://127.0.0.1:18080")
    parser.add_argument("--on-server", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return
    if args.on_server:
        on_server()
        return
    existing = runpy.run_path(str(Path(__file__).with_name("deploy-test.py")))
    import base64
    import paramiko
    password = None if args.key else os.environ.get("PONYNOTES_SSH_PASSWORD") or getpass.getpass("Test server SSH password: ")
    with paramiko.SSHClient() as client:
        client.get_host_keys().add(existing["HOST"], "ssh-ed25519", paramiko.Ed25519Key(data=base64.b64decode(existing["HOST_KEY"])))
        client.connect(existing["HOST"], username="root", password=password, key_filename=str(args.key) if args.key else None, timeout=20, auth_timeout=20)
        password = None
        client.get_transport().set_keepalive(30)
        if args.tunnel:
            serve_tunnel(client)
            return
        execute = existing["remote_run"]
        execute(client, "set -eu; command -v python3; docker compose version; free -m; df -h /opt")
        if args.check:
            return
        files = upstream_files()
        execute(client, f"set -eu; if [ -e {REMOTE}/.env ] && [ ! -e {REMOTE}/.managed-by-ponynotes ]; then echo 'Unmanaged WeKnora deployment exists'; exit 1; fi; install -d -m 700 {REMOTE} {REMOTE}/config; touch {REMOTE}/.managed-by-ponynotes")
        with client.open_sftp() as sftp:
            for target, content in files.items():
                remote = REMOTE + "/" + target
                try:
                    with sftp.open(remote, "rb") as stream:
                        old = stream.read()
                    if old != content:
                        raise RuntimeError(f"Remote upstream file was modified: {remote}")
                except FileNotFoundError:
                    sftp.putfo(io.BytesIO(content), remote)
                    sftp.chmod(remote, 0o644)
            sftp.put(str(Path(__file__)), REMOTE + "/deploy.py")
            sftp.chmod(REMOTE + "/deploy.py", 0o600)
        execute(client, f"python3 -u {REMOTE}/deploy.py --on-server")


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print("Deployment interrupted; persistent data was retained.", file=sys.stderr)
        sys.exit(1)
    except Exception as error:
        # Never dump HTTP response bodies: authentication responses contain tokens.
        print(f"Deployment failed: {error}", file=sys.stderr)
        sys.exit(1)
