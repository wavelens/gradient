# SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

import json

start_all()

machine.wait_for_unit("gradient-server.service")

with subtest("check nix"):
    machine.succeed("nix --version")

with subtest("check api health"):
    print(machine.succeed("curl http://localhost:3000/api/health -i --fail"))

with subtest("check api /user/register"):
    req = json.loads(machine.succeed("""
        curl -XPOST http://localhost:3000/api/user/register -H 'Content-Type: application/json' -d '{"username": "test", "name": "Test User", "email": "tes@were.local", "password": "password"}'
    """))

    assert req.get("error") == False, req.get("message")

    req = json.loads(machine.succeed("""
        curl -XPOST http://localhost:3000/api/user/register -H 'Content-Type: application/json' -d '{"username": "test", "name": "Test User", "email": "tes@were.local", "password": "password"}'
    """))

    assert req.get("error") == True, "User should already exist, since it was created in last request"

with subtest("check api not authorized"):
    req = json.loads(machine.succeed("""
        curl -XGET http://localhost:3000/api/organization -H 'Content-Type: application/json'
    """))

    assert req.get("error") == True, "Should not be authorized"

with subtest("check api /user/login"):
    req = json.loads(machine.succeed("""
        curl -XPOST http://localhost:3000/api/user/login -H 'Content-Type: application/json' -d '{"loginname": "test", "password": "password"}'
    """))

    assert req.get("error") == False, req.get("message")

    user_token = req.get("message")
    print(f"User token: {user_token}")

with subtest("check api user authorization"):
    req = json.loads(machine.succeed("""
        curl -XGET http://localhost:3000/api/organization -H 'Authorization: Bearer user_token' -H 'Content-Type: application/json'
    """.replace("user_token", user_token)))

    assert req.get("error") == False, req.get("message")


with subtest("check api /user/api"):
    req = json.loads(machine.succeed("""
        curl -XPOST http://localhost:3000/api/user/api -H 'Authorization: Bearer user_token' -H 'Content-Type: application/json' -d '{"name": "MyApiKey"}'
    """.replace("user_token", user_token)))

    assert req.get("error") == False, req.get("message")

    api_key = req.get("message")
    print(f"API Key: {api_key}")

with subtest("check api key authorization"):
    req = json.loads(machine.succeed("""
        curl -XGET http://localhost:3000/api/organization -H 'Authorization: Bearer api_key' -H 'Content-Type: application/json'
    """.replace("api_key", api_key)))

    assert req.get("error") == False, req.get("message")

with subtest("check api /organization"):
    req = json.loads(machine.succeed("""
        curl -XPOST http://localhost:3000/api/organization -H 'Authorization: Bearer api_key' -H 'Content-Type: application/json' -d '{"name": "MyOrganization", "description": "My Organization", "use_nix_store": true}'
    """.replace("api_key", api_key)))

    assert req.get("error") == False, req.get("message")

    org_id = req.get("message")
    print(f"Organization ID: {org_id}")

    req = json.loads(machine.succeed("""
        curl -XGET http://localhost:3000/api/organization -H 'Authorization: Bearer api_key' -H 'Content-Type: application/json'
    """.replace("api_key", api_key)))

    assert req.get("error") == False, req.get("message")
    assert len(req.get("message")) == 1, "Should have only one organization"
    assert req.get("message")[0].get("id") == org_id, "Organization ID should match"

with subtest("check api /organization/:id"):
    req = json.loads(machine.succeed("""
        curl -XGET http://localhost:3000/api/organization/org_id -H 'Authorization: Bearer api_key' -H 'Content-Type: application/json'
    """.replace("api_key", api_key).replace("org_id", org_id)))

    assert req.get("error") == False, req.get("message")
    assert req.get("message").get("id") == org_id, "Organization ID should match"

    req = json.loads(machine.succeed("""
        curl -XPOST http://localhost:3000/api/organization/org_id -H 'Authorization: Bearer api_key' -H 'Content-Type: application/json' -d '{"name": "MyProject", "description": "My Project", "repository": "git@github.com:Wavelens/Gradient.git", "evaluation_wildcard": "packages.*"}'
    """.replace("api_key", api_key).replace("org_id", org_id)))

    assert req.get("error") == False, req.get("message")

    project_id = req.get("message")
    print(f"Project ID: {project_id}")

with subtest("check api /organization/:id/ssh"):
    req = json.loads(machine.succeed("""
        curl -XGET http://localhost:3000/api/organization/org_id/ssh -H 'Authorization: Bearer api_key' -H 'Content-Type: application/json'
    """.replace("api_key", api_key).replace("org_id", org_id)))

    assert req.get("error") == False, req.get("message")

    ssh_key = req.get("message")

    assert ssh_key.startswith("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI"), f"invalid ssh-key: {ssh_key}"

    req = json.loads(machine.succeed("""
        curl -XPOST http://localhost:3000/api/organization/org_id/ssh -H 'Authorization: Bearer api_key' -H 'Content-Type: application/json'
    """.replace("api_key", api_key).replace("org_id", org_id)))

    assert req.get("error") == False, req.get("message")

    new_ssh_key = req.get("message")

    assert new_ssh_key != ssh_key, "Should have new ssh key"

    print(f"New SSH Key: {new_ssh_key}")

with subtest("check api /project/:id"):
    req = json.loads(machine.succeed("""
        curl -XGET http://localhost:3000/api/project/project_id -H 'Authorization: Bearer api_key' -H 'Content-Type: application/json'
    """.replace("api_key", api_key).replace("project_id", project_id)))

    assert req.get("error") == False, req.get("message")
    assert req.get("message").get("id") == project_id, "Project ID should match"

with subtest("check api /server"):
    req = json.loads(machine.succeed("""
        curl -XPOST http://localhost:3000/api/server -H 'Authorization: Bearer api_key' -H 'Content-Type: application/json' -d '{"name": "MyServer", "host": "localhost", "port": 22, "username": "root", "organization_id": "org_id", "architectures": ["x86_64-linux"], "features": ["big-parallel"]}'
    """.replace("api_key", api_key).replace("org_id", org_id)))

    assert req.get("error") == False, req.get("message")

    server_id = req.get("message")
