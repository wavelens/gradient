# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

# A server and a co-located worker, configured with nothing but two `enable`
# flags. Proves the module provisions the worker's credentials itself and that
# a project created afterwards picks the worker up without a registration step.

import json

API = "http://127.0.0.1:3000/api/v1"

start_all()
machine.wait_for_unit("gradient-local-worker-token.service")


def banner(msg):
    print(f"\n=== {msg} ===")


def api(method, path, token=None, body=None):
    cmd = f"curl -sS -X {method} {API}/{path}"
    if token:
        cmd += f" -H 'Authorization: Bearer {token}'"
    if body is not None:
        cmd += f" -H 'Content-Type: application/json' -d '{body}'"
    out = machine.succeed(cmd)
    j = json.loads(out)
    assert j.get("error") is False, f"{method} {path}: {j.get('message')}"
    return j.get("message")


banner("Credentials are generated, not configured")
perms = machine.succeed("stat -c '%a %U %G' /var/lib/gradient-worker/local-token").strip()
assert perms == "400 gradient-worker gradient-worker", f"token perms: {perms}"

token = machine.succeed("cat /var/lib/gradient-worker/local-token").strip()
assert len(token) == 64, f"token must be 64 base64 chars, got {len(token)}"

# The peers file is the only place the derived identity is written down, and
# the worker answers the server's challenge straight out of it.
peers = machine.succeed("cat /var/lib/gradient-worker/local-peers").strip()
identity, _, peer_token = peers.partition(":")
assert len(identity) == 36 and identity.count("-") == 4, \
    f"identity is not a UUID: {identity!r}"
assert peer_token == token, f"peers line carries a different token: {peers!r}"

banner("Server provisions the base worker from generated state")
machine.wait_for_unit("gradient-server.service")
machine.wait_for_open_port(3000)
machine.wait_for_unit("gradient-worker.service")
machine.wait_until_succeeds("journalctl -u gradient-server.service | grep -q 'Created base worker'", timeout=60)

banner("A new project enables the worker with no registration step")
admin = api("POST", "auth/basic/login", body=json.dumps({
    "loginname": "admin", "password": "admin_password"}))
assert admin, "admin login returned empty token"

api("PUT", "projects", token=admin, body=json.dumps({
    "name": "demo", "display_name": "Demo", "description": "zero-config"}))

workers = api("GET", "projects/demo/workers", token=admin)
entry = next((w for w in workers if w["worker_id"] == identity), None)
assert entry is not None, f"local worker missing from the project's worker list: {workers}"
assert entry["is_base"], "local worker should be a base worker"
assert entry["active"], "local worker should be auto-enabled for a new project"

banner("Worker authenticates and reports live")
# A base worker with no projects is rejected until one exists, so the join
# waits out the worker's reconnect backoff (60s ceiling).


def worker_is_live():
    workers = api("GET", "projects/demo/workers", token=admin)
    entry = next((w for w in workers if w["worker_id"] == identity), None)
    # `live` is omitted, not null, while the worker is disconnected.
    return entry is not None and entry.get("live") is not None


with machine.nested("waiting for the worker to connect"):
    retry(lambda _: worker_is_live(), timeout_seconds=180)

banner("An opt-out is not undone by auto_enable")
api("PATCH", f"projects/demo/workers/{identity}", token=admin,
    body=json.dumps({"active": False}))
machine.systemctl("restart gradient-server.service")
machine.wait_for_open_port(3000)
admin = api("POST", "auth/basic/login", body=json.dumps({
    "loginname": "admin", "password": "admin_password"}))
workers = api("GET", "projects/demo/workers", token=admin)
entry = next((w for w in workers if w["worker_id"] == identity), None)
assert entry is not None and not entry["active"], \
    "a project that opted out must stay opted out across a restart"

banner("Credentials survive a restart")
machine.succeed("systemctl restart gradient-local-worker-token.service")
assert machine.succeed("cat /var/lib/gradient-worker/local-token").strip() == token, \
    "token must not be regenerated once it exists"
