"""From nac: python PonyNotes-Cloud-New/deploy/47.94.42.183/test-deploy-test.py."""
import importlib.util
import json
from pathlib import Path
import tarfile
import tempfile

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("deploy_test", HERE / "deploy-test.py")
deploy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(deploy)

with tempfile.TemporaryDirectory() as temporary:
    root = Path(temporary)
    for folder in deploy.REPOS.values():
        (root / folder).mkdir()
        (root / folder / "uncommitted.go").write_text("local edit\n")
    files = [".env", "deploy.env", ".env.local", "env.deploy.secret", ".git/config", "target/output", "logs/server.log", "node_modules/pkg/index.js", ".ssh/id_ed25519", "id_rsa", "nginx/ssl/private_key.key", "deploy/47.94.42.183/deploy-test.py"]
    for name in files:
        path = root / "PonyNotes-Cloud-New" / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("must not upload\n")
    for name in [".sqlx/query.json", "migrations/20260101_test.sql"]:
        path = root / "PonyNotes-Cloud-New" / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b"keep exact bytes\r\n")
    ui = root / "PonyNotes-Web-Desktop/xmbj-www-ui"
    ui.mkdir()
    (ui / "index.vue").write_text("not a backend")
    output = root / "bundle.tar.gz"
    deploy.bundle(root, output)
    with tarfile.open(output) as archive:
        names = set(archive.getnames())
        assert {"cloud/uncommitted.go", "gotrue/uncommitted.go", "web/uncommitted.go", "tools/deploy-test.sh", "manifest.json"} <= names
        assert not any("cloud/" + name in names for name in files)
        assert not any(name.startswith("web/xmbj-www-ui") for name in names)
        assert archive.extractfile("cloud/migrations/20260101_test.sql").read() == b"keep exact bytes\r\n"
        assert json.load(archive.extractfile("manifest.json"))["target"] == "47.94.42.183"
print("PASS source snapshot includes local changes and SQLx files, excludes secrets/build output/UI")
