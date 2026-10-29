"""Server check: python3 test-otp-smoke.py. Uses dedicated disposable accounts."""
import json
from pathlib import Path
import time
import urllib.error
import urllib.request

BASE = "http://127.0.0.1"
CONFIG = dict(line.split("=", 1) for line in (Path(__file__).parent / ".env").read_text().splitlines() if line and not line.startswith("#"))


def request(path, data=None, token=None, method=None, expected=200):
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = "Bearer " + token
    req = urllib.request.Request(BASE + path, data=None if data is None else json.dumps(data).encode(), headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=30) as response:
            status, body = response.status, json.load(response)
    except urllib.error.HTTPError as error:
        status, body = error.code, json.load(error)
    assert status == expected, (path.split("?", 1)[0], status, body.get("msg", body.get("message", "")))
    return body


admin = request("/gotrue/token?grant_type=password", {"email": CONFIG["ADMIN_EMAIL"], "password": CONFIG["ADMIN_PASSWORD"]})["access_token"]
for field, account in [("email", "otp-smoke@ponynotes.test"), ("phone", "19900000002")]:
    # Match the desktop Rust SDK, which sends magiclink for both account types.
    verify_type = "magiclink"
    user_id = None
    try:
        request("/gotrue/otp", {field: account, "create_user": True, "data": {"otp_smoke": True}})
        request("/gotrue/verify", {field: account, "token": "000000", "type": verify_type}, expected=403)
        login = request("/gotrue/verify", {field: account, "token": "123456", "type": verify_type})
        user_id = login["user"]["id"]
        assert login["user"]["user_metadata"].get("otp_smoke") is True
        assert login["access_token"] and login["refresh_token"]
        print("PASS", field, "registration, wrong-code rejection and real session", flush=True)
        if field == "email":
            request("/gotrue/verify", {field: account, "token": "123456", "type": verify_type}, expected=403)
            print("PASS email code cannot be reused", flush=True)
        # Preserve the normal per-account sending interval.
        time.sleep(6)
        request("/gotrue/otp", {field: account, "create_user": False})
        again = request("/gotrue/verify", {field: account, "token": "123456", "type": verify_type})
        assert again["user"]["id"] == user_id
        refreshed = request("/gotrue/token?grant_type=refresh_token", {"refresh_token": again["refresh_token"]})
        assert refreshed["access_token"]
        request("/api/user/verify/" + again["access_token"])
        profile = request("/api/user/profile", token=again["access_token"])
        assert profile["code"] == 0
        print("PASS", field, "repeat login, refresh and Cloud profile", flush=True)
    finally:
        if user_id:
            request("/gotrue/admin/users/" + user_id, token=admin, method="DELETE")
            print("PASS", field, "temporary account removed", flush=True)
