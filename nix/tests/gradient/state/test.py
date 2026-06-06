# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

# Exercises declarative `services.gradient.state` provisioning end to end:
# resources are created from the Nix config at startup and then read back over
# the REST API and the `gradient` CLI as the provisioned users.

import json

API = "http://gradient.local/api/v1"

# Alice's state-provisioned API key (bearer = "GRAD" + the key file contents).
ALICE = "GRADa1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p6q7r8s9t0u1v2w3x4y5z6A7B8C9D0E1F2G3"


def curl(method, path, token=None, body=None):
    cmd = f"curl -sS -X {method} {API}/{path} -H 'Content-Type: application/json'"
    if token:
        cmd += f" -H 'Authorization: Bearer {token}'"
    if body is not None:
        cmd += f" -d '{json.dumps(body)}'"
    return machine.succeed(cmd)


def api(method, path, token=None, body=None, expect_error=False):
    j = json.loads(curl(method, path, token=token, body=body))
    if expect_error:
        assert j.get("error") is True, f"{method} {path}: expected error, got {j}"
    else:
        assert j.get("error") is False, f"{method} {path}: {j.get('message')}"
    return j.get("message")


def login(loginname, password):
    return api("POST", "auth/basic/login", body={"loginname": loginname, "password": password})


def names(items):
    return [i.get("name") for i in items]


def gradient(args):
    return machine.succeed(f"gradient {args}")


start_all()
machine.wait_for_unit("gradient-server.service")
machine.wait_for_unit("nginx.service")
machine.wait_for_open_port(3000)
machine.wait_for_open_port(80)

with subtest("state applied at startup"):
    machine.succeed("systemctl is-active gradient-server.service")
    logs = machine.succeed("journalctl -u gradient-server.service --no-pager")
    assert "Loading state configuration" in logs, "Should load state configuration"
    assert "State configuration validated successfully" in logs, "Should validate state"
    assert "State applied successfully" in logs, "Should apply state successfully"

with subtest("state-managed API key authenticates"):
    api("GET", "orgs", token=ALICE)

with subtest("state-managed organizations exist"):
    orgs = api("GET", "orgs", token=ALICE).get("items")
    assert "corp" in names(orgs), f"corp organization missing. Found: {names(orgs)}"

with subtest("state-managed projects exist"):
    projects = api("GET", "projects/corp", token=ALICE).get("items")
    assert "web-app" in names(projects), f"web-app missing. Found: {names(projects)}"
    assert "mobile-app" in names(projects), f"mobile-app missing. Found: {names(projects)}"

with subtest("state-managed caches exist"):
    caches = api("GET", "caches", token=ALICE)
    assert "main" in names(caches), f"main missing. Found: {names(caches)}"
    assert "dev" in names(caches), f"dev missing. Found: {names(caches)}"

with subtest("org member can view subscribed caches they do not own"):
    # bob is a plain 'corp' member who created none of the caches and holds no
    # cache membership. 'corp' subscribes to both caches, so they must be both
    # listable and viewable by name - active ('main') and inactive ('dev') alike.
    # Regression: detail lookups used to demand a direct cache membership, so an
    # org member got a 404 from the API, the `gradient` CLI and the WebUI.
    bob = login("bob", "bob_password")
    listed = names(api("GET", "caches", token=bob))
    for name in ["main", "dev"]:
        assert name in listed, f"org member should list '{name}'. Found: {listed}"
        shown = api("GET", f"caches/{name}", token=bob)
        assert shown.get("name") == name, f"show returned wrong cache: {shown}"

    # The reporter's exact reproduction path: `gradient cache show <name>`.
    gradient("config Server http://gradient.local")
    gradient(f"config AuthToken {bob}")
    gradient("organization select corp")
    assert "main" in gradient("cache show main"), "`gradient cache show main` should succeed"

with subtest("non-members cannot see private caches"):
    # charlie belongs to no organization that subscribes to the caches, so the
    # broadened read access must not leak them - neither listed nor viewable.
    api("POST", "auth/basic/register", body={
        "username": "charlie", "name": "Charlie Brown",
        "email": "charlie@example.com", "password": "SecureAuth456$",
    })
    charlie = login("charlie", "SecureAuth456$")
    assert "main" not in names(api("GET", "caches", token=charlie)), "private cache leaked in list"
    api("GET", "caches/main", token=charlie, expect_error=True)

with subtest("managed entities are read-only"):
    api("POST", "auth/basic/register", expect_error=True, body={
        "username": "alice", "name": "Alice Modified",
        "email": "alice.modified@example.com", "password": "StrongSecret123!",
    })

with subtest("organization SSH keys work"):
    ssh_key = api("GET", "orgs/corp/ssh", token=ALICE)
    assert ssh_key.startswith("ssh-ed25519 "), f"Invalid SSH key format: {ssh_key[:50]}..."

print("All state management tests passed successfully!")
