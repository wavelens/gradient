import json

start_all()

machine.wait_for_unit("network-online.target")
machine.wait_for_unit("gradient.service")

with subtest("check api health"):
    print(machine.succeed("curl http://localhost:3000/health -i --fail"))

with subtest("check api /user/register"):
    req = json.loads(machine.succeed("""
        curl -XPOST http://localhost:3000/user/register -H 'Content-Type: application/json' -d '{"username": "test", "name": "Test User", "email": "tes@were.local", "password": "password"}'
    """))

    assert req.get("error") == False, req.get("message")

    req = json.loads(machine.succeed("""
        curl -XPOST http://localhost:3000/user/register -H 'Content-Type: application/json' -d '{"username": "test", "name": "Test User", "email": "tes@were.local", "password": "password"}'
    """))

    assert req.get("error") == True, "User should already exist, since it was created in last request"

with subtest("check api not authorized"):
    req = json.loads(machine.succeed("""
        curl -XGET http://localhost:3000/organization -H 'Content-Type: application/json'
    """))

    assert req.get("error") == True, "Should not be authorized"

with subtest("check api /user/login"):
    req = json.loads(machine.succeed("""
        curl -XPOST http://localhost:3000/user/login -H 'Content-Type: application/json' -d '{"loginname": "test", "password": "password"}'
    """))

    assert req.get("error") == False, req.get("message")

    user_token = req.get("message")
    print(f"User token: {user_token}")

with subtest("check api user authorization"):
    req = json.loads(machine.succeed("""
        curl -XGET http://localhost:3000/organization -H 'Authorization: Bearer user_token' -H 'Content-Type: application/json'
    """.replace("user_token", user_token)))

    assert req.get("error") == False, req.get("message")


with subtest("check api /user/api"):
    req = json.loads(machine.succeed("""
        curl -XPOST http://localhost:3000/user/api -H 'Authorization: Bearer user_token' -H 'Content-Type: application/json' -d '{"name": "MyApiKey"}'
    """.replace("user_token", user_token)))

    assert req.get("error") == False, req.get("message")

    api_key = req.get("message")
    print(f"API Key: {api_key}")

with subtest("check api key authorization"):
    req = json.loads(machine.succeed("""
        curl -XGET http://localhost:3000/organization -H 'Authorization: Bearer api_key' -H 'Content-Type: application/json'
    """.replace("api_key", api_key)))

    assert req.get("error") == False, req.get("message")

