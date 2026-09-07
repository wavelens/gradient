# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

# gradient-deploy against a scripted API: the target's own system closure is the
# "already built" deployment and a specialisation of it is the "new" one, so a
# real switch can be asserted without a builder. The service must sit on the
# task's live WebSocket while CI runs and settle on the first event that
# resolves it, rather than reporting that nothing was built.

import base64
import json

API = "http://127.0.0.1:8090"

start_all()
machine.wait_for_unit("gradient-stub-api.service")
machine.wait_until_succeeds(f"curl -sSf {API}/api/v1/health")

BASE_SYSTEM = machine.succeed("readlink /run/current-system").strip()
DEPLOY_SYSTEM = machine.succeed("readlink -f /run/current-system/specialisation/deployed").strip()
assert BASE_SYSTEM != DEPLOY_SYSTEM, "specialisation must be a distinct closure"


def banner(msg):
    print(f"\n=== {msg} ===")


def post(path, payload=None):
    cmd = f"curl -sSf -o /dev/null -X POST {API}{path}"
    if payload is not None:
        b64 = base64.b64encode(json.dumps(payload).encode()).decode()
        cmd = f"echo {b64} | base64 -d | " + cmd + " --data-binary @-"
    machine.succeed(cmd)


def set_state(status, entry_points, evaluation_id="e1"):
    post("/control/state", {
        "evaluation": {"id": evaluation_id, "status": status},
        "entry_points": entry_points,
    })


def entry_point(path, build_status):
    return {"build_id": "00000000-0000-0000-0000-000000000001",
            "build_status": build_status,
            "outputs": {"out": path}}


def stats():
    return json.loads(machine.succeed(f"curl -sSf {API}/control/stats"))


def start_deploy():
    machine.succeed("systemctl start --no-block gradient-deploy.service")


def deploy_state():
    return machine.succeed("systemctl show gradient-deploy.service -p ActiveState --value").strip()


def await_deploy():
    machine.wait_until_succeeds(
        "systemctl show gradient-deploy.service -p ActiveState --value | grep -qx inactive"
    )
    result = machine.succeed("systemctl show gradient-deploy.service -p Result --value").strip()
    assert result == "success", f"gradient-deploy ended {result}:\n{deploy_log()}"
    return deploy_log()


def deploy_log():
    invocation = machine.succeed(
        "systemctl show gradient-deploy.service -p InvocationID --value"
    ).strip()
    return machine.succeed(f"journalctl --no-pager _SYSTEMD_INVOCATION_ID={invocation}")


def await_connected():
    machine.wait_until_succeeds(f"curl -sSf {API}/control/stats | jq -e '.connections == 1'")


def current_system():
    return machine.succeed("readlink /run/current-system").strip()


# ── Already running the evaluated system ──────────────────────────────────────
# The output path is known at evaluation time, so a target that already runs it
# is finished regardless of what the build is doing, and must not wait.
banner("Already up-to-date settles without waiting")
post("/control/reset")
set_state("Building", [entry_point(BASE_SYSTEM, "Queued")])
start_deploy()
log = await_deploy()
assert "already up-to-date" in log, log
assert stats()["connections"] == 0, f"must not open the live socket: {stats()}"

# ── Evaluation fails part-way through the build ───────────────────────────────
banner("Evaluation failing mid-build stops the wait without deploying")
post("/control/reset")
set_state("Building", [entry_point(DEPLOY_SYSTEM, "Building")], evaluation_id="e2")
start_deploy()
await_connected()
assert deploy_state() == "activating", "must still be waiting on the live socket"

set_state("Failed", [entry_point(DEPLOY_SYSTEM, "Building")], evaluation_id="e2")
log = await_deploy()
assert "finished Failed" in log, log
assert current_system() == BASE_SYSTEM, "a failed evaluation must not switch the system"

# ── Waits through evaluation and build, then deploys ──────────────────────────
banner("Waits through evaluation and build, then deploys")
post("/control/reset")
set_state("EvaluatingFlake", [], evaluation_id="e3")
start_deploy()
await_connected()
assert deploy_state() == "activating", "must wait while entry points do not exist yet"

set_state("Building", [entry_point(DEPLOY_SYSTEM, "Queued")], evaluation_id="e3")
machine.sleep(3)
assert deploy_state() == "activating", "must keep waiting while the build is queued"

set_state("Building", [entry_point(DEPLOY_SYSTEM, "Completed")], evaluation_id="e3")
log = await_deploy()
assert f"Deployment to {DEPLOY_SYSTEM} completed successfully" in log, log
machine.succeed("test -e /etc/gradient-deployed")
assert current_system() == DEPLOY_SYSTEM, f"expected {DEPLOY_SYSTEM}, got {current_system()}"

# One pass on start, one on connect, one per event: the failsafe re-check is an
# hour out, so anything beyond that would mean the service is polling.
passes = stats()["entry_points"]
assert passes <= 5, f"expected event-driven re-checks, got {passes}"
