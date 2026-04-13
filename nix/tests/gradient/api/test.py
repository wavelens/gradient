# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

import json

start_all()

machine.wait_for_unit("gradient-server.service")
machine.wait_for_open_port(3000)

with subtest("check nix"):
    machine.succeed("nix --version")

with subtest("check api health"):
    print(machine.succeed("curl http://localhost:3000/api/v1/health -i --fail"))

with subtest("check api /auth/basic/register"):
    req = json.loads(machine.succeed("""
        curl -XPOST http://localhost:3000/api/v1/auth/basic/register -H 'Content-Type: application/json' -d '{"username": "testuser", "name": "Test User", "email": "test@were.local", "password": "SecureTest123!"}'
    """))

    assert req.get("error") == False, req.get("message")

    req = json.loads(machine.succeed("""
        curl -XPOST http://localhost:3000/api/v1/auth/basic/register -H 'Content-Type: application/json' -d '{"username": "testuser", "name": "Test User", "email": "test@were.local", "password": "SecureTest123!"}'
    """))

    assert req.get("error") == True, "User should already exist, since it was created in last request"

with subtest("check api not authorized"):
    req = json.loads(machine.succeed("""
        curl -XGET http://localhost:3000/api/v1/orgs -H 'Content-Type: application/json'
    """))

    assert req.get("error") == True, "Should not be authorized"

with subtest("check api /auth/basic/login"):
    req = json.loads(machine.succeed("""
        curl -XPOST http://localhost:3000/api/v1/auth/basic/login -H 'Content-Type: application/json' -d '{"loginname": "testuser", "password": "SecureTest123!"}'
    """))

    assert req.get("error") == False, req.get("message")

    user_token = req.get("message")
    print(f"User token: {user_token}")

with subtest("check api user authorization"):
    req = json.loads(machine.succeed("""
        curl -XGET http://localhost:3000/api/v1/orgs -H 'Authorization: Bearer user_token' -H 'Content-Type: application/json'
    """.replace("user_token", user_token)))

    assert req.get("error") == False, req.get("message")


with subtest("check api /user/keys"):
    req = json.loads(machine.succeed("""
        curl -XPOST http://localhost:3000/api/v1/user/keys -H 'Authorization: Bearer user_token' -H 'Content-Type: application/json' -d '{"name": "MyApiKey"}'
    """.replace("user_token", user_token)))

    assert req.get("error") == False, req.get("message")

    api_key = req.get("message")
    print(f"API Key: {api_key}")

with subtest("check api key authorization"):
    req = json.loads(machine.succeed("""
        curl -XGET http://localhost:3000/api/v1/orgs -H 'Authorization: Bearer api_key' -H 'Content-Type: application/json'
    """.replace("api_key", api_key)))

    assert req.get("error") == False, req.get("message")

with subtest("check api /orgs"):
    req = json.loads(machine.succeed("""
        curl -XPUT http://localhost:3000/api/v1/orgs -H 'Authorization: Bearer api_key' -H 'Content-Type: application/json' -d '{"name": "myorganization", "display_name": "My Organization", "description": "My Organization"}'
    """.replace("api_key", api_key)))

    assert req.get("error") == False, req.get("message")

    org_id = req.get("message")
    print(f"Organization ID: {org_id}")

    req = json.loads(machine.succeed("""
        curl -XGET http://localhost:3000/api/v1/orgs -H 'Authorization: Bearer api_key' -H 'Content-Type: application/json'
    """.replace("api_key", api_key)))

    assert req.get("error") == False, req.get("message")
    orgs = req.get("message").get("items")
    assert len(orgs) == 1, "Should have only one organization"
    assert orgs[0].get("id") == org_id, "Organization ID should match"

with subtest("check api /orgs/{organization}"):
    org_name = "myorganization"

    req = json.loads(machine.succeed("""
        curl -XGET http://localhost:3000/api/v1/orgs/org_name -H 'Authorization: Bearer api_key' -H 'Content-Type: application/json'
    """.replace("api_key", api_key).replace("org_name", org_name)))

    assert req.get("error") == False, req.get("message")
    assert req.get("message").get("id") == org_id, "Organization ID should match"

    req = json.loads(machine.succeed("""
        curl -XPUT http://localhost:3000/api/v1/projects/org_name -H 'Authorization: Bearer api_key' -H 'Content-Type: application/json' -d '{"name": "myproject", "display_name": "My Project", "description": "My Project", "repository": "git@github.com:Wavelens/Gradient.git", "evaluation_wildcard": "packages.*"}'
    """.replace("api_key", api_key).replace("org_name", org_name)))

    assert req.get("error") == False, req.get("message")

    project_id = req.get("message")
    print(f"Project ID: {project_id}")

with subtest("check api /orgs/{organization}/ssh"):
    req = json.loads(machine.succeed("""
        curl -XGET http://localhost:3000/api/v1/orgs/org_name/ssh -H 'Authorization: Bearer api_key' -H 'Content-Type: application/json'
    """.replace("api_key", api_key).replace("org_name", org_name)))

    assert req.get("error") == False, req.get("message")

    ssh_key = req.get("message")

    assert ssh_key.startswith("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI"), f"invalid ssh-key: {ssh_key}"

    req = json.loads(machine.succeed("""
        curl -XPOST http://localhost:3000/api/v1/orgs/org_name/ssh -H 'Authorization: Bearer api_key' -H 'Content-Type: application/json'
    """.replace("api_key", api_key).replace("org_name", org_name)))

    assert req.get("error") == False, req.get("message")

    new_ssh_key = req.get("message")

    assert new_ssh_key != ssh_key, "Should have new ssh key"

    print(f"New SSH Key: {new_ssh_key}")

with subtest("check api /projects/{organization}/{project}"):
    project_name = "myproject"

    req = json.loads(machine.succeed("""
        curl -XGET http://localhost:3000/api/v1/projects/org_name/project_name -H 'Authorization: Bearer api_key' -H 'Content-Type: application/json'
    """.replace("api_key", api_key).replace("org_name", org_name).replace("project_name", project_name)))

    assert req.get("error") == False, req.get("message")
    assert req.get("message").get("id") == project_id, "Project ID should match"

with subtest("check api /orgs/{organization}/workers"):
    # Register a worker under the org
    req = json.loads(machine.succeed("""
        curl -XPOST http://localhost:3000/api/v1/orgs/org_name/workers -H 'Authorization: Bearer api_key' -H 'Content-Type: application/json' -d '{"worker_id": "test-worker-001"}'
    """.replace("api_key", api_key).replace("org_name", org_name)))

    assert req.get("error") == False, req.get("message")
    worker_reg = req.get("message")
    peer_id = worker_reg.get("peer_id")
    token = worker_reg.get("token")
    assert peer_id is not None, "peer_id should be present"
    assert token is not None and len(token) > 0, "token should be non-empty"
    assert len(token) == 64, f"Expected 64-char base64 token (48 bytes), got {len(token)}: {token}"
    print(f"Worker registered: peer_id={peer_id}")

    # List workers — should contain our registration
    req = json.loads(machine.succeed("""
        curl -XGET http://localhost:3000/api/v1/orgs/org_name/workers -H 'Authorization: Bearer api_key' -H 'Content-Type: application/json'
    """.replace("api_key", api_key).replace("org_name", org_name)))

    assert req.get("error") == False, req.get("message")
    workers = req.get("message")
    assert any(w.get("worker_id") == "test-worker-001" for w in workers), \
        f"test-worker-001 not found in: {[w.get('worker_id') for w in workers]}"

    # Delete the worker registration
    req = json.loads(machine.succeed("""
        curl -XDELETE http://localhost:3000/api/v1/orgs/org_name/workers/test-worker-001 -H 'Authorization: Bearer api_key' -H 'Content-Type: application/json'
    """.replace("api_key", api_key).replace("org_name", org_name)))

    assert req.get("error") == False, req.get("message")

    # Confirm deletion
    req = json.loads(machine.succeed("""
        curl -XGET http://localhost:3000/api/v1/orgs/org_name/workers -H 'Authorization: Bearer api_key' -H 'Content-Type: application/json'
    """.replace("api_key", api_key).replace("org_name", org_name)))

    assert req.get("error") == False, req.get("message")
    workers = req.get("message")
    assert not any(w.get("worker_id") == "test-worker-001" for w in workers), \
        "worker should have been deleted"
