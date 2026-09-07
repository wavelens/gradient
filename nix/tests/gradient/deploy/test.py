# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

# gradient-deploy against a scripted API: a target's own system closure is the
# "already built" deployment and a specialisation of it is the "new" one, so a
# real switch can be asserted without a builder. The service must sit on the
# task's live WebSocket while CI runs and settle on the first event that
# resolves it, rather than reporting that nothing was built. `poller` is the
# same target with `websockets = false`, covering the timed fallback.

import base64
import json

API = "http://127.0.0.1:8090"


def banner(msg):
    print(f"\n=== {msg} ===")


def post(node, path, payload=None):
    cmd = f"curl -sSf -o /dev/null -X POST {API}{path}"
    if payload is not None:
        b64 = base64.b64encode(json.dumps(payload).encode()).decode()
        cmd = f"echo {b64} | base64 -d | " + cmd + " --data-binary @-"
    node.succeed(cmd)


def set_state(node, status, entry_points, evaluation_id="e1"):
    post(node, "/control/state", {
        "evaluation": {"id": evaluation_id, "status": status},
        "entry_points": entry_points,
    })


def entry_point(path, build_status):
    return {"build_id": "00000000-0000-0000-0000-000000000001",
            "build_status": build_status,
            "outputs": {"out": path}}


def stats(node):
    return json.loads(node.succeed(f"curl -sSf {API}/control/stats"))


def start_deploy(node):
    node.succeed("systemctl start --no-block gradient-deploy.service")


def deploy_state(node):
    return node.succeed("systemctl show gradient-deploy.service -p ActiveState --value").strip()


def deploy_log(node):
    invocation = node.succeed(
        "systemctl show gradient-deploy.service -p InvocationID --value"
    ).strip()
    return node.succeed(f"journalctl --no-pager _SYSTEMD_INVOCATION_ID={invocation}")


def await_deploy(node):
    node.wait_until_succeeds(
        "systemctl show gradient-deploy.service -p ActiveState --value | grep -qx inactive"
    )
    result = node.succeed("systemctl show gradient-deploy.service -p Result --value").strip()
    assert result == "success", f"gradient-deploy ended {result}:\n{deploy_log(node)}"
    return deploy_log(node)


def await_connected(node):
    node.wait_until_succeeds(f"curl -sSf {API}/control/stats | jq -e '.connections == 1'")


def current_system(node):
    return node.succeed("readlink /run/current-system").strip()


def boot(node):
    node.wait_for_unit("gradient-stub-api.service")
    node.wait_until_succeeds(f"curl -sSf {API}/api/v1/health")
    base = current_system(node)
    deployed = node.succeed("readlink -f /run/current-system/specialisation/deployed").strip()
    assert base != deployed, "specialisation must be a distinct closure"
    return base, deployed


start_all()
BASE_SYSTEM, DEPLOY_SYSTEM = boot(machine)

# ── Already running the evaluated system ──────────────────────────────────────
# The output path is known at evaluation time, so a target that already runs it
# is finished regardless of what the build is doing, and must not wait.
banner("Already up-to-date settles without waiting")
post(machine, "/control/reset")
set_state(machine, "Building", [entry_point(BASE_SYSTEM, "Queued")])
start_deploy(machine)
log = await_deploy(machine)
assert "already up-to-date" in log, log
assert stats(machine)["connections"] == 0, f"must not open the live socket: {stats(machine)}"

# ── Evaluation fails part-way through the build ───────────────────────────────
banner("Evaluation failing mid-build stops the wait without deploying")
post(machine, "/control/reset")
set_state(machine, "Building", [entry_point(DEPLOY_SYSTEM, "Building")], evaluation_id="e2")
start_deploy(machine)
await_connected(machine)
assert deploy_state(machine) == "activating", "must still be waiting on the live socket"

set_state(machine, "Failed", [entry_point(DEPLOY_SYSTEM, "Building")], evaluation_id="e2")
log = await_deploy(machine)
assert "finished Failed" in log, log
assert current_system(machine) == BASE_SYSTEM, "a failed evaluation must not switch the system"

# ── Waits through evaluation and build, then deploys ──────────────────────────
banner("Waits through evaluation and build, then deploys")
post(machine, "/control/reset")
set_state(machine, "EvaluatingFlake", [], evaluation_id="e3")
start_deploy(machine)
await_connected(machine)
assert deploy_state(machine) == "activating", "must wait while entry points do not exist yet"

set_state(machine, "Building", [entry_point(DEPLOY_SYSTEM, "Queued")], evaluation_id="e3")
machine.sleep(3)
assert deploy_state(machine) == "activating", "must keep waiting while the build is queued"

set_state(machine, "Building", [entry_point(DEPLOY_SYSTEM, "Completed")], evaluation_id="e3")
log = await_deploy(machine)
assert f"Deployment to {DEPLOY_SYSTEM} completed successfully" in log, log
machine.succeed("test -e /etc/gradient-deployed")
assert current_system(machine) == DEPLOY_SYSTEM, f"expected {DEPLOY_SYSTEM}, got {current_system(machine)}"

# One pass on start, one on connect, one per event. There is no timed re-check,
# so anything beyond that would mean the service is polling.
passes = stats(machine)["entry_points"]
assert passes <= 5, f"expected event-driven re-checks, got {passes}"

# ── The fallback for networks without WebSockets ──────────────────────────────
banner("With websockets disabled the wait falls back to re-checking on a timer")
POLLER_SYSTEM, _ = boot(poller)
post(poller, "/control/reset")
set_state(poller, "Building", [], evaluation_id="e4")
start_deploy(poller)
poller.sleep(6)
assert deploy_state(poller) == "activating", "must wait for the evaluation without a socket"
assert stats(poller)["connections"] == 0, "must not open a socket when websockets are off"
assert stats(poller)["entry_points"] >= 2, "must keep re-checking on the timer"

set_state(poller, "Building", [entry_point(POLLER_SYSTEM, "Queued")], evaluation_id="e4")
log = await_deploy(poller)
assert "already up-to-date" in log, log
