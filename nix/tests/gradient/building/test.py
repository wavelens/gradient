import json

start_all()

buildMachine.wait_for_unit("network-online.target")
server.wait_for_unit("network-online.target")
server.wait_for_unit("gradient.service")
server.succeed("curl http://localhost:3000/health -i --fail")

with subtest("check jwt login"):
    user = machine.succeed("""
        curl -XPOST http://localhost:3000/user/login -H 'Content-Type: application/json' -d '{"loginname": "test", "password": "password"}'
    """)
    userj = json.loads(user)

    assert userj.get("error") == False, userj.get("message")

