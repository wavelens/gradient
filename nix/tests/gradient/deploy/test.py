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


def entry_point(path, build_status, eval=""):
    return {"build_id": "00000000-0000-0000-0000-000000000001",
            "eval": eval,
            "build_status": build_status,
            "outputs": {"out": path}}


def microvm_entry_point(name, path, build_status):
    return entry_point(
        path,
        build_status,
        f"nixosConfigurations.{name}.config.microvm.declaredRunner",
    )


def stats(node):
    return json.loads(node.succeed(f"curl -sSf {API}/control/stats"))


def start_deploy(node):
    node.succeed("systemctl reset-failed gradient-deploy.service")
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


def await_deploy_failure_from_journal(node, message):
    node.wait_until_succeeds(
        f"journalctl -u gradient-deploy --no-pager | grep -q 'Error: {message}'"
    )
    node.wait_until_succeeds(
        "journalctl -u gradient-deploy --no-pager | grep -q 'Failed with result'"
    )
    return deploy_log(node)


def await_connected(node):
    node.wait_until_succeeds(f"curl -sSf {API}/control/stats | jq -e '.connections == 1'")


def current_system(node):
    return node.succeed("readlink /run/current-system").strip()


def microvm_runner(node, name="guest", link="current"):
    return node.succeed(
        f"readlink -f /var/lib/microvms/{name}/{link}"
    ).strip()


def boot(node):
    node.wait_for_unit("gradient-stub-api.service")
    node.wait_until_succeeds(f"curl -sSf {API}/api/v1/health")
    base = current_system(node)
    deployed = node.succeed("readlink -f /run/current-system/specialisation/deployed").strip()
    assert base != deployed, "specialisation must be a distinct closure"
    return base, deployed


start_all()
BASE_SYSTEM, DEPLOY_SYSTEM = boot(machine)
MICROVM_RUNNERS = json.loads(microvm.succeed("cat /etc/microvm-runners"))
MICROVM_OLD_RUNNER = MICROVM_RUNNERS["old"]
MICROVM_NEW_RUNNER = MICROVM_RUNNERS["new"]
MICROVM_NEWER_RUNNER = MICROVM_RUNNERS["newer"]

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
assert "finished: Failed" in log, log
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

# ── A system no substituter can supply ────────────────────────────────────────
banner("A system no substituter can supply")
post(machine, "/control/reset")
unfetchable = "/nix/store/" + "a" * 32 + DEPLOY_SYSTEM[DEPLOY_SYSTEM.index("-"):]
set_state(machine, "Building", [entry_point(unfetchable, "Completed")], evaluation_id="e5")
start_deploy(machine)
log = await_deploy_failure_from_journal(
    machine, f"cannot fetch {unfetchable} from a substituter"
)
assert f"Error: cannot fetch {unfetchable} from a substituter" in log, log
assert "completed successfully" not in log, log
assert current_system(machine) == DEPLOY_SYSTEM, "a failed fetch must not switch the system"

# ── MicroVM deployment installs the correct runner ──────────────────────
banner("MicroVM deployment installs the exact selected runner and restarts it")
microvm.wait_for_unit("gradient-stub-api.service")
microvm.wait_until_succeeds(f"curl -sSf {API}/api/v1/health")

# A configured VM missing from the host must fail before querying Gradient and
# must not be recreated by the deployer.
banner("An undeployed MicroVM exits early without creating state")
microvm.succeed(
    "mv /var/lib/microvms/second-guest /var/lib/microvms/second-guest.not-deployed"
)
post(microvm, "/control/reset")
set_state(microvm, "Building", [
    microvm_entry_point("guest", MICROVM_NEW_RUNNER, "Completed"),
    microvm_entry_point("second-guest", MICROVM_NEWER_RUNNER, "Completed"),
])
start_deploy(microvm)
log = await_deploy_failure_from_journal(microvm, "MicroVM second-guest is not deployed")
assert stats(microvm)["evaluations"] == 0
assert stats(microvm)["entry_points"] == 0
microvm.succeed("test ! -e /var/lib/microvms/second-guest")
microvm.succeed(
    "mv /var/lib/microvms/second-guest.not-deployed /var/lib/microvms/second-guest"
)

assert microvm_runner(microvm, "guest") == MICROVM_OLD_RUNNER
assert microvm_runner(microvm, "second-guest") == MICROVM_OLD_RUNNER
post(microvm, "/control/reset")
set_state(microvm, "Building", [
    microvm_entry_point("guest", MICROVM_NEW_RUNNER, "Completed"),
    microvm_entry_point("second-guest", MICROVM_NEWER_RUNNER, "Completed"),
])
start_deploy(microvm)
log = await_deploy(microvm)
assert f"Deployment of MicroVM guest to {MICROVM_NEW_RUNNER} completed successfully" in log, log
assert f"Deployment of MicroVM second-guest to {MICROVM_NEWER_RUNNER} completed successfully" in log, log
assert microvm_runner(microvm, "guest") == MICROVM_NEW_RUNNER
assert microvm_runner(microvm, "guest", "booted") == MICROVM_NEW_RUNNER
assert microvm_runner(microvm, "second-guest") == MICROVM_NEWER_RUNNER
assert microvm_runner(microvm, "second-guest", "booted") == MICROVM_NEWER_RUNNER
assert "active" == microvm.succeed(
    "systemctl show microvm@guest.service -p ActiveState --value"
).strip()
assert "active" == microvm.succeed(
    "systemctl show microvm@second-guest.service -p ActiveState --value"
).strip()
microvm.succeed(
    f"test \"$(readlink -f /nix/var/nix/gcroots/microvm/guest)\" = {MICROVM_NEW_RUNNER}"
)
microvm.succeed(
    f"test \"$(readlink -f /nix/var/nix/gcroots/microvm/old-guest)\" = {MICROVM_OLD_RUNNER}"
)
microvm.succeed(
    f"test \"$(readlink -f /nix/var/nix/gcroots/microvm/second-guest)\" = {MICROVM_NEWER_RUNNER}"
)
microvm.succeed(
    f"test \"$(readlink -f /nix/var/nix/gcroots/microvm/old-second-guest)\" = {MICROVM_OLD_RUNNER}"
)

# Current and booted runner links plus an active unit settle without waiting for
# another build or restarting the VM.
banner("An already-running MicroVM runner settles without waiting")
post(microvm, "/control/reset")
set_state(microvm, "Building", [
    microvm_entry_point("guest", MICROVM_NEW_RUNNER, "Queued"),
    microvm_entry_point("second-guest", MICROVM_NEWER_RUNNER, "Queued"),
])
start_deploy(microvm)
log = await_deploy(microvm)
assert "already up-to-date" in log, log
assert stats(microvm)["connections"] == 0

# A terminal evaluation must stop waiting even if a MicroVM build status is
# still nonterminal.
banner("A failed MicroVM evaluation stops waiting for its build")
post(microvm, "/control/reset")
set_state(microvm, "Building", [
    microvm_entry_point("guest", MICROVM_OLD_RUNNER, "Building"),
    microvm_entry_point("second-guest", MICROVM_NEWER_RUNNER, "Completed"),
], evaluation_id="e6")
start_deploy(microvm)
await_connected(microvm)
set_state(microvm, "Failed", [
    microvm_entry_point("guest", MICROVM_OLD_RUNNER, "Building"),
    microvm_entry_point("second-guest", MICROVM_NEWER_RUNNER, "Completed"),
], evaluation_id="e6")
log = await_deploy(microvm)
assert "was still Building when evaluation finished Failed" in log, log
assert "New deployment found for MicroVM guest" not in log, log
assert microvm_runner(microvm, "guest") == MICROVM_NEW_RUNNER

# A missing runner cannot change current or cause a restart.
banner("A MicroVM runner no substituter can supply")
unfetchable_runner = "/nix/store/" + "a" * 32 + "-microvm-runner-unfetchable"
post(microvm, "/control/reset")
set_state(microvm, "Building", [
    microvm_entry_point("guest", unfetchable_runner, "Completed"),
    microvm_entry_point("second-guest", MICROVM_NEWER_RUNNER, "Completed"),
])
start_deploy(microvm)
log = await_deploy_failure_from_journal(
    microvm, f"cannot fetch {unfetchable_runner} from a substituter"
)
assert microvm_runner(microvm, "guest") == MICROVM_NEW_RUNNER
assert microvm_runner(microvm, "guest", "booted") == MICROVM_NEW_RUNNER

# A realized path that is not a MicroVM runner is rejected before changing links.
banner("A non-runner output cannot be activated as a MicroVM")
invalid_runner = MICROVM_RUNNERS["invalid"]
post(microvm, "/control/reset")
set_state(microvm, "Building", [
    microvm_entry_point("guest", invalid_runner, "Completed"),
    microvm_entry_point("second-guest", MICROVM_NEWER_RUNNER, "Completed"),
])
start_deploy(microvm)
log = await_deploy_failure_from_journal(
    microvm, "MicroVM runner .* has no executable bin/microvm-run"
)
assert microvm_runner(microvm, "guest") == MICROVM_NEW_RUNNER
assert microvm_runner(microvm, "guest", "booted") == MICROVM_NEW_RUNNER

# The upstream toplevel marker identifies MicroVMs managed declaratively.
banner("A declarative MicroVM is rejected")
microvm.succeed("touch /var/lib/microvms/second-guest/toplevel")
post(microvm, "/control/reset")
set_state(microvm, "Building", [
    microvm_entry_point("guest", MICROVM_OLD_RUNNER, "Completed"),
    microvm_entry_point("second-guest", MICROVM_NEWER_RUNNER, "Completed"),
])
start_deploy(microvm)
log = await_deploy_failure_from_journal(microvm, "MicroVM second-guest is declarative")
assert microvm_runner(microvm, "guest") == MICROVM_NEW_RUNNER
assert microvm_runner(microvm, "second-guest") == MICROVM_NEWER_RUNNER
microvm.succeed("rm /var/lib/microvms/second-guest/toplevel")

# A restart failure is reported, with the previous runner retained for recovery.
banner("A failed MicroVM restart preserves the previous runner")
microvm.succeed("touch /run/fail-microvm-restart")
post(microvm, "/control/reset")
set_state(microvm, "Building", [
    microvm_entry_point("guest", MICROVM_OLD_RUNNER, "Completed"),
    microvm_entry_point("second-guest", MICROVM_NEWER_RUNNER, "Completed"),
])
start_deploy(microvm)
log = await_deploy_failure_from_journal(microvm, "restarting MicroVM guest failed")
assert microvm_runner(microvm, "guest") == MICROVM_OLD_RUNNER
assert microvm_runner(microvm, "guest", "old") == MICROVM_NEW_RUNNER
assert microvm_runner(microvm, "guest", "booted") == MICROVM_NEW_RUNNER
microvm.succeed(
    f"test \"$(readlink -f /nix/var/nix/gcroots/microvm/old-guest)\" = {MICROVM_NEW_RUNNER}"
)

# A second failed restart must not replace the last known-good runner.
banner("A repeated MicroVM restart failure preserves the previous runner")
start_deploy(microvm)
log = await_deploy_failure_from_journal(microvm, "restarting MicroVM guest failed")
assert microvm_runner(microvm, "guest") == MICROVM_OLD_RUNNER
assert microvm_runner(microvm, "guest", "old") == MICROVM_NEW_RUNNER
assert microvm_runner(microvm, "guest", "booted") == MICROVM_NEW_RUNNER
microvm.succeed(
    f"test \"$(readlink -f /nix/var/nix/gcroots/microvm/old-guest)\" = {MICROVM_NEW_RUNNER}"
)

# A later run retries activation if current was updated but the VM did not restart.
banner("A failed MicroVM restart can be retried")
microvm.succeed("rm /run/fail-microvm-restart")
start_deploy(microvm)
log = await_deploy(microvm)
assert "completed successfully" in log, log
assert microvm_runner(microvm, "guest") == MICROVM_OLD_RUNNER
assert microvm_runner(microvm, "guest", "booted") == MICROVM_OLD_RUNNER
assert microvm_runner(microvm, "guest", "old") == MICROVM_NEW_RUNNER
microvm.succeed(
    f"test \"$(readlink -f /nix/var/nix/gcroots/microvm/old-guest)\" = {MICROVM_NEW_RUNNER}"
)
