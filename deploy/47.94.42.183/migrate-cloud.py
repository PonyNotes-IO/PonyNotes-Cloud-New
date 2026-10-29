"""Apply this repository's mixed 8/14 digit SQLx versions in calendar order."""
import hashlib
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parent
COMMAND = ["docker", "compose", "exec", "-T", "postgres", "psql", "-U", "postgres", "-d", "ponynotes", "-v", "ON_ERROR_STOP=1", "-At"]


def sql(text):
    result = subprocess.run(COMMAND, cwd=ROOT, input=text, text=True, capture_output=True)
    if result.returncode:
        raise RuntimeError(result.stderr[-2000:])
    return result.stdout


sql("""CREATE TABLE IF NOT EXISTS _sqlx_migrations (
    version bigint PRIMARY KEY,
    description text NOT NULL,
    installed_on timestamptz NOT NULL DEFAULT now(),
    success boolean NOT NULL,
    checksum bytea NOT NULL,
    execution_time bigint NOT NULL
);""")
applied = {}
for row in sql("SELECT version, encode(checksum,'hex') FROM _sqlx_migrations WHERE success;").splitlines():
    version, checksum = row.split("|")
    applied[int(version)] = checksum
paths = sorted((ROOT / "source/migrations").glob("*.sql"), key=lambda path: (path.name.split("_", 1)[0].ljust(14, "0"), path.name))
for path in paths:
    version_text, description = path.stem.split("_", 1)
    assert len(version_text) in (8, 14), path.name
    version = int(version_text)
    checksum = hashlib.sha384(path.read_bytes()).hexdigest()
    if version in applied:
        assert checksum == applied[version], "Applied migration checksum changed: " + path.name
        continue
    # Several repository migrations contain their own transaction wrappers.
    body = path.read_bytes().decode("utf-8")
    body = re.sub(r"(?m)^(?:BEGIN|COMMIT);\r?$", "", body)
    description = description.replace("_", " ").replace("'", "''")
    record = f"INSERT INTO _sqlx_migrations(version,description,success,checksum,execution_time) VALUES ({version},'{description}',true,decode('{checksum}','hex'),0);"
    sql("BEGIN;\n" + body + "\n;\n" + record + "\nCOMMIT;")
    print("APPLIED", path.name, flush=True)
print("Cloud migrations complete")
