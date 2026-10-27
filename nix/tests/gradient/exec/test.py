# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

# Exec jobs in cloud-hypervisor guests on two workers: `stopper` stops its guests with the
# worker, `keeper` keeps them. Covers guest units, pre-warm and pause, the PreWarmVms clamp
# and the surplus stop, the virtiofs store and work shares, tap networking with NAT and the
# guest DNS forwarder, the agent protocol, exit codes, retries, maxSilent, abort, both restart
# modes with log replay and JobLost from a restarted agent. Not covered: VFIO, USB/IP,
# /dev relays, the nested kvm device, multi-node jobs, custom images. The CI host must offer
# nested KVM (kvm_intel or kvm_amd with nested=1), since the workers boot guests in a VM.

import json
import time

API = "http://gradient.local/api/v1"
GUESTS = "/var/lib/gradient-worker/guests"
WORKERS = [stopper, keeper]
NAMES = ["fetch", "silent", "abort", "long-stop", "fail", "long-keep", "lost"]
TERMINAL = {"completed", "failed", "aborted"}

token = None
eval_id = None
JOBS = {}


def banner(msg):
    print(f"\n=== {msg} ===")


def fleet_tail():
    parts = []
    for node in WORKERS:
        parts.append(f"--- {node.name} worker journal ---\n" + node.execute(
            "journalctl -u gradient-worker --no-pager -n 60")[1])
        parts.append(f"--- {node.name} guest consoles ---\n" + node.execute(
            f"tail -n 15 {GUESTS}/*/console.log 2>/dev/null")[1])
    return "\n".join(parts)


def wait_for(what, check, timeout=300):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(1)
    raise Exception(f"{what}: not within {timeout}s\n{fleet_tail()}")


def api(method, path, body=None):
    cmd = f"curl -sS -X {method} {API}/{path}"
    if token:
        cmd += f" -H 'Authorization: Bearer {token}'"
    if body is not None:
        cmd += f" -H 'Content-Type: application/json' -d '{json.dumps(body)}'"
    reply = json.loads(server.succeed(cmd))
    assert reply.get("error") is False, f"{method} {path}: {reply.get('message')}"
    return reply["message"]


def job(name):
    return api("GET", f"exec-jobs/{JOBS[name]}")


def log(name, attempt=None):
    query = f"?attempt={attempt}" if attempt else ""
    return api("GET", f"exec-jobs/{JOBS[name]}/log{query}")


def end_reasons(state):
    return [attempt["end_reason"] for attempt in state["attempts"]]


def wait_status(name, status, timeout=600):
    def check():
        state = job(name)
        ended = all(attempt["ended_at"] for attempt in state["attempts"])
        if state["status"] == status and ended:
            return state
        if state["status"] in TERMINAL and state["status"] != status:
            raise Exception(
                f"{name} ended {state['status']}, expected {status}: {json.dumps(state)}\n"
                f"log:\n{log(name)}\n{fleet_tail()}"
            )
        return None

    return wait_for(f"{name} reaches {status}", check, timeout)


def wait_running(name, tick):
    def check():
        state = job(name)
        if state["status"] in TERMINAL:
            raise Exception(f"{name} ended {state['status']} before tick {tick}: {json.dumps(state)}")
        return state["status"] == "running" and f"tick {tick}" in log(name).splitlines()

    wait_for(f"{name} runs and prints tick {tick}", check)


def active_guests(node):
    listing = node.succeed(
        "systemctl list-units --plain --no-legend --state=active 'gradient-guest-*.service'")
    return [line.split()[0] for line in listing.splitlines() if line.strip()]


def guest_id(unit):
    return unit.removeprefix("gradient-guest-").removesuffix(".service")


def vm_state(node, unit):
    status, state = node.execute(
        f"curl -sf --unix-socket {GUESTS}/{guest_id(unit)}/api.sock "
        "http://localhost/api/v1/vm.info | jq -r .state")
    return state.strip() if status == 0 else None


def one_paused_guest(node):
    units = active_guests(node)
    return len(units) == 1 and vm_state(node, units[0]) == "Paused"


def guest_of_job(node, job_id):
    return node.succeed(
        f"for f in {GUESTS}/*/state.json; do "
        f"jq -e --arg id {job_id} '.job.job_id == $id' $f >/dev/null && basename $(dirname $f); "
        "done; true").strip()


def main_pid(node, guest):
    return node.succeed(f"systemctl show -P MainPID gradient-guest-{guest}.service").strip()


start_all()

banner("Phase 1: the exec workers have nested KVM")
for node in WORKERS:
    node.wait_for_unit("multi-user.target")
    status, _ = node.execute("test -c /dev/kvm")
    assert status == 0, (
        f"{node.name} has no /dev/kvm. The CI host must offer nested KVM "
        "(kvm_intel or kvm_amd loaded with nested=1)"
    )

banner("Phase 2: each worker connects with one paused warm guest")
server.wait_for_unit("gradient-server.service")
for node in WORKERS:
    node.wait_for_unit("gradient-worker.service")
    node.wait_until_succeeds(
        "journalctl -u gradient-worker --no-pager | grep -q 'handshake successful'", timeout=180)
for node in WORKERS:
    wait_for(f"{node.name} holds one paused warm guest", lambda: one_paused_guest(node))

banner("Phase 3: the programs wait in the upstream cache")
server.succeed(
    "nix --extra-experimental-features nix-command copy "
    "--to 'file:///srv/upstream?secret-key=/etc/gradient/secrets/upstream_key' "
    f"--no-check-sigs {PROGRAMS}"
)
server.succeed("chown -R nginx:nginx /srv/upstream")

banner("Phase 4: push the repository with the exec apps")
server.succeed(f"{GIT} config --global init.defaultBranch main")
server.succeed(f"{GIT} config --global user.email 'nixos@localhost'")
server.succeed(f"{GIT} config --global user.name 'NixOS test'")
server.succeed(f"{GIT} init /var/lib/git/test")
server.succeed("cp /var/lib/git/flake.nix /var/lib/git/flake.lock /var/lib/git/apps.nix /var/lib/git/test/")
server.succeed("chmod u+w /var/lib/git/test/*")
server.succeed(f"sed -i 's#\\[hash\\]#{NIXPKGS_HASH}#g' /var/lib/git/test/flake.lock")
server.succeed(f"{GIT} -C /var/lib/git/test add flake.nix flake.lock apps.nix")
server.succeed(f"{GIT} -C /var/lib/git/test commit -m 'exec apps'")
server.succeed("chown -R git:git /var/lib/git/test")

banner("Phase 5: the evaluation records every exec job")
server.wait_for_open_port(80)
token = api("POST", "auth/basic/login", {"loginname": "admin", "password": "admin_password"})
eval_id = wait_for(
    "the task starts an evaluation",
    lambda: api("GET", "tasks/project/task").get("last_evaluation"),
)


def recorded_jobs():
    status = api("GET", f"evals/{eval_id}")["status"]
    assert status not in ("Failed", "Aborted"), f"evaluation {eval_id} ended {status}"
    jobs = api("GET", f"evals/{eval_id}/exec-jobs")
    by_name = {entry["entry_point"].rsplit(".", 1)[-1]: entry["id"] for entry in jobs}
    return by_name if set(NAMES) <= by_name.keys() else None


JOBS = wait_for("the evaluation records every exec job", recorded_jobs, timeout=900)

banner("Phase 6: upcoming exec jobs raise the warm target up to maxPreWarm")
stopper.wait_until_succeeds(
    "journalctl -u gradient-worker --no-pager | grep 'warm guest target changed' | grep -q 'target=2'",
    timeout=300,
)

banner("Phase 7: a networked job resolves a name and reaches the server")
fetched = wait_status("fetch", "completed")
assert end_reasons(fetched) == ["completed"], json.dumps(fetched)
output = log("fetch")
assert "served to the guest" in output, f"fetch did not reach the server:\n{output}"
assert f"job={JOBS['fetch']} attempt=1" in output, f"fetch saw no job context:\n{output}"

banner("Phase 8: a silent program times out")
silent = wait_status("silent", "failed")
assert end_reasons(silent) == ["timeout"], json.dumps(silent)
assert "program produced no output for 5s (maxSilent exceeded)" in log("silent"), log("silent")

banner("Phase 9: an aborted job stops")
wait_running("abort", 3)
api("POST", f"exec-jobs/{JOBS['abort']}/abort")
aborted = wait_status("abort", "aborted", timeout=120)
assert end_reasons(aborted) == ["aborted"], json.dumps(aborted)

banner("Phase 10: a worker stopping its guests stops the guest of a running job")
wait_running("long-stop", 5)
stop_guest = guest_of_job(stopper, JOBS["long-stop"])
assert stop_guest, "no guest state names the long-stop job"
stopper.succeed("systemctl restart gradient-worker.service")
stopper.wait_until_fails(f"systemctl is-active --quiet gradient-guest-{stop_guest}.service", timeout=60)

banner("Phase 11: a failing job uses its retry")
failed = wait_status("fail", "failed")
assert end_reasons(failed) == ["failed", "failed"], json.dumps(failed)
assert failed["retries_left"] == 0, json.dumps(failed)
assert "attempt 2 exits with 3" in log("fail", attempt=2), log("fail", attempt=2)

banner("Phase 12: a worker keeping its guests resumes the job, a restarted agent loses its job")
wait_running("long-keep", 5)
wait_running("lost", 5)
keep_guest = guest_of_job(keeper, JOBS["long-keep"])
lost_guest = guest_of_job(keeper, JOBS["lost"])
assert keep_guest and lost_guest, f"guests of the kept jobs: {keep_guest!r} {lost_guest!r}"
pid = main_pid(keeper, keep_guest)
keeper.succeed("systemctl stop gradient-worker.service")
keeper.succeed(f"touch {GUESTS}/{lost_guest}/work/restart-agent")
keeper.wait_until_fails(f"test -e {GUESTS}/{lost_guest}/work/restart-agent", timeout=60)
keeper.succeed("systemctl start gradient-worker.service")
keeper.wait_until_succeeds(
    "journalctl -u gradient-worker --no-pager | grep -q 'resuming an exec job in its guest'", timeout=60)
assert main_pid(keeper, keep_guest) == pid, "the kept guest unit changed its main process"

kept = wait_status("long-keep", "completed")
assert end_reasons(kept) == ["completed"], json.dumps(kept)
lines = log("long-keep").splitlines()
uneven = [n for n in range(1, 151) if lines.count(f"tick {n}") != 1]
assert not uneven, f"ticks missing or repeated after the replay: {uneven}"

lost = wait_status("lost", "completed")
assert end_reasons(lost) == ["lost", "completed"], json.dumps(lost)
assert lost["retries_left"] == 0, json.dumps(lost)
assert "lost the job" in (lost["attempts"][0]["message"] or ""), json.dumps(lost)

banner("Phase 13: the stopped attempt ran again without using a retry")
restarted = wait_status("long-stop", "completed")
assert end_reasons(restarted) == ["lost", "completed"], json.dumps(restarted)
assert restarted["retries_left"] == 0, json.dumps(restarted)

banner("Phase 14: the workers settle on one paused warm guest")
for node in WORKERS:
    wait_for(f"{node.name} settles on one paused warm guest", lambda: one_paused_guest(node))
status, _ = keeper.execute("journalctl -u gradient-worker --no-pager | grep -q 'warm guest target changed'")
assert status != 0, "keeper with maxPreWarm = 0 changed its warm target"
