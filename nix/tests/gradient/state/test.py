# SPDX-FileCopyrightText: 2025 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

import time
import json

start_all()
machine.wait_for_unit("gradient-server.service")

with subtest("check service started successfully with state"):
    machine.succeed("systemctl is-active gradient-server.service")
    time.sleep(2)

    logs = machine.succeed("journalctl -u gradient-server.service --no-pager")
    assert "Loading state configuration" in logs, "Should load state configuration"
    assert "State configuration validated successfully" in logs, "Should validate state"
    assert "State applied successfully" in logs, "Should apply state successfully"

with subtest("check state-managed API keys work"):
    alice_token = "GRADa1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p6q7r8s9t0u1v2w3x4y5z6A7B8C9D0E1F2G3"

    req = json.loads(machine.succeed("""
        curl -XGET http://gradient.local/api/v1/orgs -H 'Authorization: Bearer alice_token' -H 'Content-Type: application/json'
    """.replace("alice_token", alice_token)))

    assert req.get("error") == False, f"API key authentication failed: {req.get('message')}"
    alice_token_to_use = alice_token

with subtest("check state-managed organizations exist"):
    req = json.loads(machine.succeed("""
        curl -XGET http://gradient.local/api/v1/orgs -H 'Authorization: Bearer alice_token' -H 'Content-Type: application/json'
    """.replace("alice_token", alice_token_to_use)))

    assert req.get("error") == False, f"Failed to get organizations: {req.get('message')}"
    orgs = req.get("message")

    corp_org = None
    for org in orgs:
        if org.get("name") == "corp":
            corp_org = org
            break

    assert corp_org is not None, "corp organization should exist"
    assert corp_org.get("name") == "corp", f"Wrong organization name: {corp_org.get('name')}"

with subtest("check state-managed projects exist"):
    req = json.loads(machine.succeed("""
        curl -XGET http://gradient.local/api/v1/projects/corp -H 'Authorization: Bearer alice_token' -H 'Content-Type: application/json'
    """.replace("alice_token", alice_token_to_use)))

    assert req.get("error") == False, f"Failed to get projects: {req.get('message')}"
    projects = req.get("message")

    project_names = [p.get("name") for p in projects]
    assert "web-app" in project_names, f"web-app project missing. Found: {project_names}"
    assert "mobile-app" in project_names, f"mobile-app project missing. Found: {project_names}"

with subtest("check state-managed servers exist"):
    req = json.loads(machine.succeed("""
        curl -XGET http://gradient.local/api/v1/servers/corp -H 'Authorization: Bearer alice_token' -H 'Content-Type: application/json'
    """.replace("alice_token", alice_token_to_use)))

    assert req.get("error") == False, f"Failed to get servers: {req.get('message')}"
    servers = req.get("message")

    server_names = [s.get("name") for s in servers]
    assert "build-server-1" in server_names, f"build-server-1 missing. Found: {server_names}"
    assert "mac-mini-farm" in server_names, f"mac-mini-farm missing. Found: {server_names}"

with subtest("check state-managed caches exist"):
    req = json.loads(machine.succeed("""
        curl -XGET http://gradient.local/api/v1/caches -H 'Authorization: Bearer alice_token' -H 'Content-Type: application/json'
    """.replace("alice_token", alice_token_to_use)))

    assert req.get("error") == False, f"Failed to get caches: {req.get('message')}"
    caches = req.get("message")

    cache_names = [c.get("name") for c in caches]
    assert "main" in cache_names, f"main missing. Found: {cache_names}"
    assert "dev" in cache_names, f"dev missing. Found: {cache_names}"

with subtest("check managed entities are read-only"):
    # Try to register with an existing state-managed username
    req = json.loads(machine.succeed("""
        curl -XPOST http://gradient.local/api/v1/auth/basic/register -H 'Content-Type: application/json' -d '{"username": "alice", "name": "Alice Modified", "email": "alice.modified@example.com", "password": "StrongSecret123!"}'
    """))

    assert req.get("error") == True, "Should not be able to register with a state-managed username"

with subtest("check non-managed users can still be created"):
    req = json.loads(machine.succeed("""
        curl -XPOST http://gradient.local/api/v1/auth/basic/register -H 'Content-Type: application/json' -d '{"username": "charlie", "name": "Charlie Brown", "email": "charlie@example.com", "password": "SecureAuth456$"}'
    """))

    assert req.get("error") == False, f"Should be able to create non-managed user: {req.get('message')}"

    req = json.loads(machine.succeed("""
        curl -XPOST http://gradient.local/api/v1/auth/basic/login -H 'Content-Type: application/json' -d '{"loginname": "charlie", "password": "SecureAuth456$"}'
    """))

    assert req.get("error") == False, f"Charlie login failed: {req.get('message')}"

with subtest("check organization SSH keys work"):
    req = json.loads(machine.succeed("""
        curl -XGET http://gradient.local/api/v1/orgs/corp/ssh -H 'Authorization: Bearer alice_token' -H 'Content-Type: application/json'
    """.replace("alice_token", alice_token_to_use)))

    assert req.get("error") == False, f"Failed to get SSH key: {req.get('message')}"
    ssh_key = req.get("message")

    assert ssh_key.startswith("ssh-ed25519 "), f"Invalid SSH key format: {ssh_key[:50]}..."

print("All state management tests passed successfully!")
