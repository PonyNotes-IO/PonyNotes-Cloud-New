"""Deploy the three local backend repositories to the existing test server."""
import argparse
import base64
from datetime import datetime
import getpass
import hashlib
import io
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tarfile
import tempfile
import time
import uuid

HOST = "47.94.42.183"
REMOTE = "/opt/ponynotes"
# Pin the test server's public host key; never silently trust a replacement.
HOST_KEY = "AAAAC3NzaC1lZDI1NTE5AAAAINg4eb9hNVV8BEJcwMo+r/QgRxYbnZ8fPeftEVJvf7y0"
HERE = Path(__file__).resolve().parent
REPOS = {"cloud": "PonyNotes-Cloud-New", "gotrue": "PonyNotes-gotrue", "web": "PonyNotes-Web-Desktop"}
EXCLUDE = {".git", ".ssh", ".idea", ".vscode", ".zcode", "target", "build", "node_modules", "vendor", "__pycache__", ".dart_tool", "logs", "data"}
TOOLS = ["deploy-test.sh", "build-cloud.sh", "build-gotrue.sh", "Dockerfile.cloud", "Dockerfile.gotrue", ".dockerignore", "migrate-cloud.py", "smoke.py", "test-otp-smoke.py"]


def include_source(info):
    parts = Path(info.name).parts
    name = parts[-1]
    if any(part in EXCLUDE for part in parts) or info.issym() or info.islnk():
        return None
    if parts[0] == "web" and len(parts) > 1 and parts[1] in {"ruoyi-ui", "xmbj-www-ui"}:
        return None
    if parts[:2] in {("cloud", "nginx"), ("cloud", "deploy")} or name.startswith(("id_rsa", "id_ed25519")):
        return None
    if name == ".env" or name.endswith(".env") or name.startswith(".env.") or ".secret" in name:
        return None
    if name == "admin-credentials.txt" or name.endswith((".jar", ".tar", ".tar.gz", ".zip", ".log")):
        return None
    return info


def bundle(workspace, output):
    manifest = {"target": HOST, "includes_local_worktree": True, "sources": {}}
    for alias, folder in REPOS.items():
        repo = workspace / folder
        if not repo.is_dir():
            raise FileNotFoundError(repo)
        revision = subprocess.run(["git", "-C", str(repo), "rev-parse", "HEAD"], capture_output=True, text=True)
        manifest["sources"][alias] = {"repository": folder, "commit": revision.stdout.strip() if revision.returncode == 0 else "unknown"}
    with tarfile.open(output, "w:gz") as archive:
        for alias, folder in REPOS.items():
            archive.add(workspace / folder, arcname=alias, filter=include_source)
        for name in TOOLS:
            archive.add(HERE / name, arcname="tools/" + name)
        content = json.dumps(manifest, indent=2).encode()
        info = tarfile.TarInfo("manifest.json")
        info.size = len(content)
        archive.addfile(info, io.BytesIO(content))


def remote_run(client, command, stdin=None):
    channel = client.get_transport().open_session()
    channel.set_combine_stderr(True)
    channel.exec_command(command)
    if stdin is not None:
        channel.sendall(stdin)
        channel.shutdown_write()
    for line in channel.makefile("rb"):
        print(line.decode("utf-8", errors="replace"), end="", flush=True)
    status = channel.recv_exit_status()
    if status:
        raise RuntimeError(f"Remote command failed (exit {status})")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workspace", type=Path, default=HERE.parents[2])
    parser.add_argument("--key", type=Path, help="SSH private key; otherwise prompt once for the root password")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="Only check SSH and server prerequisites")
    mode.add_argument("--bundle-only", type=Path, help="Create a source archive without connecting to the server")
    args = parser.parse_args()
    if args.bundle_only:
        bundle(args.workspace.resolve(), args.bundle_only)
        print("Source archive:", args.bundle_only.resolve())
        return

    import paramiko  # Installed dependency; no local Rust/Go/Java toolchain required.

    password = None if args.key else os.environ.get("PONYNOTES_SSH_PASSWORD") or getpass.getpass(f"root@{HOST} SSH password: ")
    with paramiko.SSHClient() as client:
        client.get_host_keys().add(HOST, "ssh-ed25519", paramiko.Ed25519Key(data=base64.b64decode(HOST_KEY)))
        client.connect(HOST, username="root", password=password, key_filename=str(args.key) if args.key else None, timeout=20, auth_timeout=20)
        password = None
        client.get_transport().set_keepalive(30)
        script = (HERE / "deploy-test.sh").read_bytes().replace(b"\r\n", b"\n")
        remote_run(client, "bash -n", script)
        remote_run(client, "bash -s -- --check", script)
        if args.check:
            return

        release = "test-" + datetime.now().strftime("%Y%m%d-%H%M%S-") + uuid.uuid4().hex[:8]
        destination = REMOTE + "/releases/" + release
        with tempfile.TemporaryDirectory(prefix="ponynotes-deploy-") as temporary:
            archive = Path(temporary) / "source.tar.gz"
            print("Packaging local sources (including uncommitted changes)...", flush=True)
            bundle(args.workspace.resolve(), archive)
            with archive.open("rb") as source:
                digest = hashlib.file_digest(source, "sha256").hexdigest()
            remote_run(client, shlex.join(["mkdir", "-p", "-m", "700", destination]))
            last_report = 0

            def progress(sent, total):
                nonlocal last_report
                if time.monotonic() - last_report > 10 or sent == total:
                    print(f"Upload: {sent / 1048576:.1f}/{total / 1048576:.1f} MiB", flush=True)
                    last_report = time.monotonic()

            with client.open_sftp() as sftp:
                sftp.put(str(archive), destination + "/source.tar.gz", callback=progress)
            command = f"cd {shlex.quote(destination)} && echo '{digest}  source.tar.gz' | sha256sum -c - && tar -xzf source.tar.gz --no-same-owner && bash tools/deploy-test.sh {shlex.quote(release)}"
            remote_run(client, command)
        print("Deployment complete: http://" + HOST, flush=True)


if __name__ == "__main__":
    try:
        main()
    except (Exception, KeyboardInterrupt) as error:
        print("Deployment failed:", error, file=sys.stderr)
        sys.exit(1)
