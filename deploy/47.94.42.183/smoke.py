"""Run on the server: cd /opt/ponynotes && python3 smoke.py."""
import json
import base64
import http.client
import os
from pathlib import Path
import subprocess
import urllib.error
import urllib.request
import urllib.parse
import uuid

ROOT = Path(__file__).resolve().parent
CONFIG = dict(line.split("=", 1) for line in (ROOT / ".env").read_text().splitlines() if line and not line.startswith("#"))
BASE = "http://127.0.0.1"


def request(path, data=None, token=None, label=None, method=None):
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = "Bearer " + token
    req = urllib.request.Request(BASE + path, data=None if data is None else json.dumps(data).encode(), headers=headers, method=method)
    with urllib.request.urlopen(req, timeout=60) as response:
        raw = response.read()
        body = {} if raw.strip() == b"OK" else json.loads(raw)
    assert body.get("code", 0) in (0, 200), str(body)[:300]
    print("PASS", label or path)
    return body


def expect_http(path, status, **kwargs):
    try:
        request(path, **kwargs)
    except urllib.error.HTTPError as error:
        assert error.code == status, (path, error.code)
    else:
        raise AssertionError("Expected HTTP " + str(status) + " for " + path)
    print("PASS HTTP", status, path.split("?", 1)[0])


request("/health")
request("/api/server")
request("/gotrue/health")
request("/prod-api/api/health")
login = request("/gotrue/token?grant_type=password", {"email": CONFIG["ADMIN_EMAIL"], "password": CONFIG["ADMIN_PASSWORD"]}, label="Cloud administrator login")
cloud_token = login["access_token"]
status_path = "/gotrue/user/password/status?" + urllib.parse.urlencode({"email": CONFIG["ADMIN_EMAIL"]})
assert request(status_path)["password_is_set"] is True
assert request("/gotrue/user/auth-info", token=cloud_token)["has_password"] is True
expect_http("/gotrue/user/password/status", 400)
expect_http(status_path + "&phone=8613800000000", 400)
expect_http("/gotrue/user/password/status?email=deployment-missing-" + uuid.uuid4().hex + "@ponynotes.local", 404)
expect_http("/gotrue/user/auth-info", 401)

# Confirmed temporary users exercise password setup without sending email/SMS.
test_email = "deployment-smoke-" + uuid.uuid4().hex + "@ponynotes.local"
test_password = uuid.uuid4().hex + "A1!"
test_user = request("/gotrue/admin/users", {"email": test_email, "password": test_password, "email_confirm": True}, token=cloud_token, label="Create temporary authentication check user")
try:
    test_status = "/gotrue/user/password/status?" + urllib.parse.urlencode({"email": test_email})
    assert request(test_status)["password_is_set"] is True
    test_login = request("/gotrue/token?grant_type=password", {"email": test_email, "password": test_password}, label="Temporary user password login")
    new_password = uuid.uuid4().hex + "A1!"
    request("/gotrue/user", {"password": new_password}, token=test_login["access_token"], method="PUT", label="Authenticated password update")
    expect_http("/gotrue/token?grant_type=password", 400, data={"email": test_email, "password": test_password})
    request("/gotrue/token?grant_type=password", {"email": test_email, "password": new_password}, label="Updated password login")
finally:
    request("/gotrue/admin/users/" + test_user["id"], token=cloud_token, method="DELETE", label="Remove temporary authentication check user")

request("/api/user/verify/" + cloud_token, label="Cloud user initialization")
profile = request("/api/user/profile", token=cloud_token)
assert profile["data"]["password_is_set"] is True, "Password administrator reported as having no password"
workspace = request("/api/user/workspace", token=cloud_token)["data"]["visiting_workspace"]["workspace_id"]
request("/api/workspace", token=cloud_token)
request("/api/subscription/plans", token=cloud_token)
request("/gotrue/token?grant_type=refresh_token", {"refresh_token": login["refresh_token"]}, label="Cloud token refresh")

blob_path = "/api/file_storage/" + workspace + "/blob/deployment-smoke-" + uuid.uuid4().hex
blob = b"PonyNotes storage smoke check\n"
headers = {"Authorization": "Bearer " + cloud_token, "Content-Type": "text/plain"}
upload = urllib.request.Request(BASE + blob_path, data=blob, headers=headers, method="PUT")
with urllib.request.urlopen(upload, timeout=30) as response:
    assert json.load(response)["code"] == 0
try:
    with urllib.request.urlopen(urllib.request.Request(BASE + blob_path, headers=headers), timeout=30) as response:
        assert response.read() == blob
finally:
    with urllib.request.urlopen(urllib.request.Request(BASE + blob_path, headers=headers, method="DELETE"), timeout=30) as response:
        assert json.load(response)["code"] == 0
print("PASS Cloud file upload, download and cleanup")

connection = http.client.HTTPConnection("127.0.0.1", timeout=15)
query = urllib.parse.urlencode({"token": cloud_token, "deviceId": "deployment-smoke", "clientId": "123456789"})
connection.request("GET", "/ws/v2/" + workspace + "?" + query, headers={"Connection": "Upgrade", "Upgrade": "websocket", "Sec-WebSocket-Version": "13", "Sec-WebSocket-Key": base64.b64encode(os.urandom(16)).decode()})
assert connection.getresponse().status == 101, "WebSocket upgrade failed"
connection.close()
print("PASS authenticated Cloud WebSocket upgrade")

captcha = request("/prod-api/captchaImage")
answer = subprocess.check_output(["docker", "compose", "exec", "-T", "redis", "redis-cli", "-n", "1", "--raw", "GET", "captcha_codes:" + captcha["uuid"]], cwd=ROOT, text=True, stdin=subprocess.DEVNULL).strip()
assert answer, "Captcha missing from Redis"
try:
    answer = str(json.loads(answer))
except json.JSONDecodeError:
    pass
web_token = request("/prod-api/login", {"username": "admin", "password": CONFIG["WEB_ADMIN_PASSWORD"], "code": answer, "uuid": captcha["uuid"]}, label="Web administrator login with captcha")["token"]
request("/prod-api/getInfo", token=web_token)
request("/prod-api/getRouters", token=web_token)
plans = request("/prod-api/api/ponynotes/plans/list", label="Java reads Cloud PostgreSQL subscription plans")
assert plans["rows"], "No subscription plans returned from PostgreSQL"

expect_http("/api/user/profile", 401)
print("Deployment checks passed")
