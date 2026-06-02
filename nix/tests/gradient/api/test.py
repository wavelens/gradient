# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

# Exercises the Gradient REST API surface on a single node - every management
# endpoint is hit directly (curl) and, where the CLI exposes it, also through
# `gradient`. Resources are created at runtime so the creation endpoints are
# tested too. No worker / build store is present, so build-dependent endpoints
# are checked for correct empty/not-found behaviour. Endpoints needing external
# services (OIDC, SMTP e-mail verification, forge webhooks, proto websockets,
# build-request dispatch) have dedicated tests and are out of scope here.

import json

API = "http://gradient.local/api/v1"
CLI = "gradient"

start_all()
machine.wait_for_unit("gradient-server.service")
machine.wait_for_open_port(3000)
machine.wait_for_unit("nginx.service")


def banner(msg):
    print(f"\n=== {msg} ===")


def curl(method, path, token=None, body=None, headers=None):
    cmd = f"curl -sS -X {method} {API}/{path}"
    if token:
        cmd += f" -H 'Authorization: Bearer {token}'"
    if body is not None:
        cmd += f" -H 'Content-Type: application/json' -d '{body}'"
    for h in headers or []:
        cmd += f" -H '{h}'"
    return machine.succeed(cmd)


def api(method, path, token=None, body=None, expect_error=False):
    out = curl(method, path, token=token, body=body)
    j = json.loads(out)
    if expect_error:
        assert j.get("error") is True, f"{method} {path}: expected error, got {out}"
    else:
        assert j.get("error") is False, f"{method} {path}: {j.get('message')}"
    return j.get("message")


def cli(args):
    return machine.succeed(f"{CLI} --json {args}")


# ── Phase 0: health ───────────────────────────────────────────────────────────
banner("Phase 0: health")
print(machine.succeed(f"curl -sS --fail {API}/health -i"))
api("GET", "health")

# ── Phase 1: auth + registration ──────────────────────────────────────────────
banner("Phase 1: auth")
api("POST", "auth/check-username", body=json.dumps({"username": "operator"}))  # available

api("POST", "auth/basic/register", body=json.dumps({
    "username": "operator", "name": "Operator User",
    "email": "operator@gradient.local", "password": "SecureTest123!",
}))
api("POST", "auth/basic/register", expect_error=True, body=json.dumps({
    "username": "operator", "name": "Operator User",
    "email": "operator@gradient.local", "password": "SecureTest123!",
}))
api("POST", "auth/check-username", expect_error=True,
    body=json.dumps({"username": "operator"}))  # now taken

api("GET", "orgs", expect_error=True)  # no token -> rejected

token = api("POST", "auth/basic/login", body=json.dumps({
    "loginname": "operator", "password": "SecureTest123!",
}))
assert token, "login returned empty token"
api("GET", "orgs", token=token)  # token authorizes

# Public/unauthenticated-ish reads.
print(machine.succeed(f"curl -sS {API}/config -i"))
machine.execute(f"curl -sS {API}/metrics -o /dev/null -w '%{{http_code}}'")

# ── Phase 2: user + api keys ──────────────────────────────────────────────────
banner("Phase 2: user + api keys")
me = api("GET", "user", token=token)
assert me.get("username") == "operator", me

api("PATCH", "user/settings", token=token, body=json.dumps({"name": "Admin Renamed"}))
assert api("GET", "user", token=token).get("name") == "Admin Renamed"

api("GET", "user/keys/permissions", token=token)
api("GET", "user/sessions", token=token)
api("GET", "user/audit-log", token=token)
api("GET", "user/search?q=operator", token=token)

key_token = api("POST", "user/keys", token=token, body=json.dumps({
    "name": "ci-key", "permissions": ["viewOrg"]}))
assert key_token.startswith("GRAD"), key_token
keys = api("GET", "user/keys", token=token)
key_id = next(k["id"] for k in keys if k["name"] == "ci-key")
# The created key authorizes API calls just like a session token.
api("GET", "orgs", token=key_token)
api("POST", f"user/keys/{key_id}/revoke", token=token)
api("DELETE", "user/keys", token=token, body=json.dumps({"name": "ci-key"}))

# ── Phase 3: organizations (direct + CLI) ─────────────────────────────────────
banner("Phase 3: organizations")
org_id = api("PUT", "orgs", token=token, body=json.dumps({
    "name": "myorg", "display_name": "My Org", "description": "desc"}))
assert api("GET", "orgs/myorg", token=token)["id"] == org_id
orgs = api("GET", "orgs", token=token)["items"]
assert any(o["id"] == org_id for o in orgs)
api("GET", "orgs/available", token=token)
api("GET", "orgs/public", token=token)
api("PATCH", "orgs/myorg", token=token, body=json.dumps({"display_name": "My Org 2"}))
assert api("GET", "orgs/myorg", token=token)["display_name"] == "My Org 2"

ssh_key = api("GET", "orgs/myorg/ssh", token=token)
assert ssh_key.startswith("ssh-ed25519 "), ssh_key
assert api("POST", "orgs/myorg/ssh", token=token) != ssh_key, "ssh key should rotate"

api("GET", "orgs/myorg/users", token=token)
api("GET", "orgs/myorg/subscribe", token=token)

role_id = api("POST", "orgs/myorg/roles", token=token, body=json.dumps({
    "name": "viewers", "permissions": ["viewOrg"]}))["id"]
api("GET", "orgs/myorg/roles", token=token)
api("GET", f"orgs/myorg/roles/{role_id}", token=token)
api("PATCH", f"orgs/myorg/roles/{role_id}", token=token, body=json.dumps({"name": "viewers2"}))
api("DELETE", f"orgs/myorg/roles/{role_id}", token=token)

# CLI: configure, then create/list/show/delete a second org.
machine.succeed(f"{CLI} config Server http://gradient.local")
machine.succeed(f"{CLI} config AuthToken {token}")
cli("organization create --name cliorg --display-name 'CLI Org' --description d")
api("GET", "orgs/cliorg", token=token)  # CLI-created org visible via API
cli("organization list")
cli("organization select cliorg")
cli("organization show")
cli("organization delete")
api("GET", "orgs/cliorg", token=token, expect_error=True)  # gone
machine.succeed(f"{CLI} organization select myorg")

# ── Phase 4: projects (direct + CLI) ──────────────────────────────────────────
banner("Phase 4: projects")
proj_id = api("PUT", "projects/myorg", token=token, body=json.dumps({
    "name": "myproject", "display_name": "My Project", "description": "d",
    "repository": "git@github.com:Wavelens/Gradient.git", "wildcard": "packages.*"}))
assert api("GET", "projects/myorg/myproject", token=token)["id"] == proj_id
assert any(p["id"] == proj_id for p in api("GET", "projects/myorg", token=token)["items"])
api("GET", "projects/myorg/available", token=token)
api("GET", "projects/myorg/myproject/details", token=token)
api("PATCH", "projects/myorg/myproject", token=token, body=json.dumps({"display_name": "MP2"}))
api("GET", "projects/myorg/myproject/entry-points", token=token)
api("GET", "projects/myorg/myproject/metrics", token=token)
assert api("GET", "projects/myorg/myproject/evaluations", token=token) == [], \
    "fresh project should have no evaluations"

trig = api("POST", "projects/myorg/myproject/triggers", token=token, body=json.dumps({
    "type": "polling", "config": {"interval_secs": 3600}}))
api("GET", "projects/myorg/myproject/triggers", token=token)

api("GET", "projects/myorg/myproject/actions", token=token)
api("POST", "projects/myorg/myproject/active", token=token)   # enable
api("DELETE", "projects/myorg/myproject/active", token=token)  # disable
# These reach out to / depend on a completed evaluation or return non-JSON
# (SVG badge); just confirm the endpoints respond rather than asserting a body.
machine.execute(f"curl -sS -X POST -H 'Authorization: Bearer {token}' {API}/projects/myorg/myproject/check-repository")
machine.execute(f"curl -sS -H 'Authorization: Bearer {token}' {API}/projects/myorg/myproject/flake-inputs")
machine.execute(f"curl -sS -H 'Authorization: Bearer {token}' {API}/projects/myorg/myproject/badge")

# CLI: create/list/show/delete a second project under the selected org.
machine.succeed(f"{CLI} project select myproject")
cli("project create --name cliproject --display-name 'CLI Project' --description d "
    "--repository git@github.com:Wavelens/Gradient.git --wildcard 'packages.*'")
api("GET", "projects/myorg/cliproject", token=token)
cli("project list")
machine.succeed(f"{CLI} project select cliproject")
cli("project show")
cli("project delete")
api("GET", "projects/myorg/cliproject", token=token, expect_error=True)
machine.succeed(f"{CLI} project select myproject")

# ── Phase 5: workers (direct + CLI) ───────────────────────────────────────────
banner("Phase 5: workers")
reg = api("POST", "orgs/myorg/workers", token=token, body=json.dumps({
    "worker_id": "b0000000-0000-0000-0000-000000000001",
    "display_name": "api-worker"}))
assert reg.get("token") and len(reg["token"]) == 64, reg
assert any(w["worker_id"] == "b0000000-0000-0000-0000-000000000001"
           for w in api("GET", "orgs/myorg/workers", token=token))
api("GET", "orgs/myorg/workers/b0000000-0000-0000-0000-000000000001", token=token)
api("PATCH", "orgs/myorg/workers/b0000000-0000-0000-0000-000000000001", token=token,
    body=json.dumps({"display_name": "api-worker-2"}))
api("DELETE", "orgs/myorg/workers/b0000000-0000-0000-0000-000000000001", token=token)

cli("worker register c0000000-0000-0000-0000-000000000002 --display-name cli-worker")
assert any(w["worker_id"] == "c0000000-0000-0000-0000-000000000002"
           for w in api("GET", "orgs/myorg/workers", token=token))
cli("worker list")
cli("worker delete c0000000-0000-0000-0000-000000000002")
assert not any(w["worker_id"] == "c0000000-0000-0000-0000-000000000002"
               for w in api("GET", "orgs/myorg/workers", token=token))

# ── Phase 6: caches (direct + CLI) ────────────────────────────────────────────
banner("Phase 6: caches")
api("PUT", "caches", token=token, body=json.dumps({
    "name": "maincache", "display_name": "Main", "description": "d", "priority": 10}))
api("GET", "caches/maincache", token=token)
assert any(c["name"] == "maincache" for c in api("GET", "caches", token=token))
api("GET", "caches/available", token=token)
api("GET", "caches/public", token=token)
api("GET", "caches/maincache/public-key", token=token)
api("GET", "caches/maincache/key", token=token)
api("GET", "caches/maincache/stats", token=token)
api("GET", "caches/maincache/members", token=token)
api("GET", "caches/maincache/roles", token=token)
api("GET", "caches/maincache/upstreams", token=token)
api("PATCH", "caches/maincache", token=token, body=json.dumps({"priority": 20}))
# Subscribe the org so the cache is usable in org context.
api("POST", "orgs/myorg/subscribe/maincache", token=token)

cli("cache create --name clicache --display-name 'CLI Cache' --description d --priority 5")
api("GET", "caches/clicache", token=token)
cli("cache list")
cli("cache show clicache")
cli("cache delete clicache")
api("GET", "caches/clicache", token=token, expect_error=True)

# ── Phase 7: cache NAR upload surface (direct + CLI) ──────────────────────────
# No nix store is needed: the endpoint validates byte length + store-path shape,
# not NAR content, so a synthetic NAR + narinfo exercises the whole surface.
banner("Phase 7: cache NAR upload / list / show / stats / delete")
assert api("GET", "caches/maincache/nars", token=token)["items"] == [], "cache starts empty"

cli_hash = "00000000000000000000000000000000"
payload = "gradient-test-nar-payload"
size = len(payload)
machine.succeed(f"printf '%s' '{payload}' > /tmp/cli.nar")
zero64 = "0" * 64
machine.succeed(
    "cat > /tmp/cli.narinfo <<'EOF'\n"
    f"StorePath: /nix/store/{cli_hash}-test\n"
    "URL: nar/cli.nar\n"
    "Compression: none\n"
    f"FileHash: sha256:{zero64}\n"
    f"FileSize: {size}\n"
    f"NarHash: sha256:{zero64}\n"
    f"NarSize: {size}\n"
    "References: \n"
    "EOF"
)
cli("cache upload --nar-file /tmp/cli.nar --narinfo /tmp/cli.narinfo maincache")

nars = api("GET", "caches/maincache/nars", token=token)["items"]
assert any(n["hash"] == cli_hash for n in nars), f"uploaded NAR missing: {nars}"
api("GET", f"caches/maincache/nars/{cli_hash}", token=token)
api("GET", "caches/maincache/nars/stats", token=token)
api("GET", f"caches/maincache/nars/available?hash={cli_hash}", token=token)
cli("cache nar list maincache")
cli(f"cache nar show maincache {cli_hash}")
cli("cache nar stats maincache")

# Second NAR via direct multipart upload.
direct_hash = "11111111111111111111111111111111"
machine.succeed(f"printf '%s' '{payload}' > /tmp/direct.nar")
narinfo_json = json.dumps({
    "store_path": f"/nix/store/{direct_hash}-test",
    "file_hash": f"sha256:{zero64}", "file_size": size,
    "nar_hash": f"sha256:{zero64}", "nar_size": size,
    "references": [], "deriver": None,
})
machine.succeed(f"cat > /tmp/direct.narinfo.json <<'EOF'\n{narinfo_json}\nEOF")
machine.succeed(
    f"curl -sS --fail -X POST {API}/caches/maincache/nars "
    f"-H 'Authorization: Bearer {token}' "
    "-F 'narinfo=</tmp/direct.narinfo.json;type=application/json' "
    "-F 'nar=@/tmp/direct.nar'"
)
nars = api("GET", "caches/maincache/nars", token=token)["items"]
assert any(n["hash"] == direct_hash for n in nars), f"direct-uploaded NAR missing: {nars}"

# Delete one via CLI, one via direct API.
cli(f"cache nar delete maincache {cli_hash} -y")
api("DELETE", f"caches/maincache/nars/{direct_hash}", token=token)
remaining = api("GET", "caches/maincache/nars", token=token)["items"]
assert not any(n["hash"] in (cli_hash, direct_hash) for n in remaining), remaining

# ── Phase 7b: cache active / public toggles ───────────────────────────────────
banner("Phase 7b: cache active / public toggles")
api("POST", "caches/maincache/public", token=token)
api("DELETE", "caches/maincache/public", token=token)
api("POST", "caches/maincache/active", token=token)
api("DELETE", "caches/maincache/active", token=token)

# ── Phase 8: build-dependent endpoints (no builds present) ────────────────────
banner("Phase 8: build-dependent endpoints respond on empty state")
missing = "00000000-0000-0000-0000-0000000000ff"
api("GET", f"evals/{missing}", token=token, expect_error=True)
api("GET", f"evals/{missing}/builds", token=token, expect_error=True)
api("GET", f"builds/{missing}", token=token, expect_error=True)
api("GET", f"builds/{missing}/graph", token=token, expect_error=True)
api("GET", f"commits/{missing}", token=token, expect_error=True)

# ── Phase 9: logout ───────────────────────────────────────────────────────────
banner("Phase 9: logout")
api("POST", "auth/logout", token=token)

banner("API test PASSED")
