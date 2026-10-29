# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

# Exec jobs in cloud-hypervisor guests on two workers: `stopper` stops its guests with the
# worker, `keeper` keeps them. Covers guest units, pre-warm and pause, the PreWarmVms clamp
# and the surplus stop, the virtiofs store and work shares, tap networking with NAT and the
# guest DNS forwarder, the agent protocol, exit codes, retries, maxSilent, abort, both restart
# modes with log replay and JobLost from a restarted agent and the SSH gateway after a
# resume: attach to the program terminal, a new shell, the task login, a forward to a
# declared port and a refused forward, secrets from the project store, a repository sops file
# and the worker sops files with masked output and a secret name in two sources, a phase, an
# uploaded output with a range download, the tree patch, the job token behind `gradient build`
# and its revocation, a two-node job across both workers on routed guest subnets, a custom
# image booted cold on the worker that allows custom images with the program started in the
# flake directory, a custom image without the guest agent, a guest booted cold with its own
# CPUs, memory and `/work` disk size and a guest size no worker can hold. Not covered: the
# browser terminal and the serial console (no browser in the test), VFIO, USB/IP, /dev relays,
# the nested kvm device. The CI host must offer nested KVM (kvm_intel or kvm_amd with
# nested=1), since the workers boot guests in a VM.

import ipaddress
import json
import re
import shlex
import time

API = "http://gradient.local/api/v1"
GUESTS = "/var/lib/gradient-worker/guests"
GUEST_UNIT = re.compile(r"gradient-guest-[0-9a-f]{32}\.service")
WORKERS = [stopper, keeper]
NAMES = [
    "fetch", "silent", "abort", "long-stop", "fail", "long-keep", "lost", "interactive", "terminal",
    "secrets", "clash", "outputs", "pair", "sized", "toolarge",
]
GUEST_SUBNETS = [ipaddress.ip_network("10.233.0.0/16"), ipaddress.ip_network("10.234.0.0/16")]
IMAGE_NAMES = ["trainer", "noagent"]
NO_GUEST_AGENT = "guest image has no gradient agent, import the gradient.guest module"
OUTPUT_BYTES = 3000000
SSH = "ssh -p 2222 -i /root/.ssh/gradient -o StrictHostKeyChecking=no -o BatchMode=yes"
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
            f"for f in {GUESTS}/*/console.log; do tr -cd '\\11\\12\\40-\\176' < $f | tail -n 40; done")[1])
        parts.append(f"--- {node.name} guest units ---\n" + node.execute(
            f"ls -ln {GUESTS}/*/; for s in {GUESTS}/*/api.sock; do "
            "curl -s --max-time 5 --unix-socket $s http://localhost/api/v1/vm.info | jq -c .state; done; "
            "journalctl -u 'gradient-guest-*' --no-pager -n 60")[1])
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
    return api("GET", f"exec/{JOBS[name]}")


def job_log(name, attempt=None, node=None):
    params = [f"{key}={value}" for key, value in [("attempt", attempt), ("node", node)] if value is not None]
    query = f"?{'&'.join(params)}" if params else ""
    return api("GET", f"exec/{JOBS[name]}/log{query}")


def sql(query):
    return server.succeed(f"runuser -u postgres -- psql -v ON_ERROR_STOP=1 -d gradient -At -c {shlex.quote(query)}").strip()


def subnet_of(address):
    return next(net for net in GUEST_SUBNETS if ipaddress.ip_address(address) in net)


def download(path, target, extra=""):
    status = server.succeed(
        f"curl -sS -o {target} -D {target}.headers -w '%{{http_code}}' {extra} "
        f"-H 'Authorization: Bearer {token}' {API}/{path}"
    )
    return status.strip(), server.succeed(f"cat {target}.headers")


def end_reasons(state):
    return [attempt["end_reason"] for attempt in state["attempts"]]


def unbuilt_image_derivations(state):
    if "image build failed" not in json.dumps(state):
        return ""
    return "--- derivations of the image evaluation no cache holds ---\n" + sql(
        "SELECT db.status, d.name, db.unneeded_outputs, left(coalesce(("
        "  SELECT string_agg(DISTINCT pd.name || '[' || coalesce(array_to_string(e.dependency_outputs, ','), 'every') "
        "         || ']', ' ') FROM derivation_dependency e "
        "  JOIN derivation_build p ON p.derivation = e.derivation JOIN derivation pd ON pd.id = e.derivation "
        "  WHERE e.dependency = d.id AND p.wanted), ''), 300) "
        "FROM build_job bj JOIN derivation_build db ON db.id = bj.derivation_build "
        "JOIN derivation d ON d.id = bj.derivation "
        f"WHERE bj.evaluation = '{images_eval}' AND db.wanted AND NOT db.cache_available "
        "AND db.status NOT IN (3, 7) ORDER BY db.status = 4 DESC, d.name LIMIT 40"
    )


def wait_status(name, status, timeout=600):
    def check():
        state = job(name)
        ended = all(attempt["ended_at"] for attempt in state["attempts"])
        if state["status"] == status and ended:
            return state
        if state["status"] in TERMINAL and state["status"] != status:
            raise Exception(
                f"{name} ended {state['status']}, expected {status}: {json.dumps(state)}\n"
                f"{unbuilt_image_derivations(state)}\n"
                f"log:\n{job_log(name)}\n{fleet_tail()}"
            )
        return None

    return wait_for(f"{name} reaches {status}", check, timeout)


def wait_running(name, tick):
    def check():
        state = job(name)
        if state["status"] in TERMINAL:
            raise Exception(f"{name} ended {state['status']} before tick {tick}: {json.dumps(state)}")
        return state["status"] == "running" and f"tick {tick}" in job_log(name).splitlines()

    wait_for(f"{name} runs and prints tick {tick}", check)


def wait_started(name):
    def check():
        state = job(name)
        if state["status"] in TERMINAL:
            raise Exception(f"{name} ended {state['status']} before a login: {json.dumps(state)}")
        return state if state["status"] == "running" else None

    return wait_for(f"{name} runs", check)


def active_guests(node):
    listing = node.succeed(
        "systemctl list-units --plain --no-legend --state=active 'gradient-guest-*.service'")
    units = [line.split()[0] for line in listing.splitlines() if line.strip()]
    return [unit for unit in units if GUEST_UNIT.fullmatch(unit)]


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
        f"jq -e --arg id exec:{job_id} '.job.job_id == $id' $f >/dev/null && basename $(dirname $f); "
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
server.wait_for_open_port(80)
token = api("POST", "auth/basic/login", {"loginname": "admin", "password": "admin_password"})
api("PUT", "projects/project/secrets/clash-key", {"value": "clash-value-1"})
server.succeed(f"{GIT} config --global init.defaultBranch main")
server.succeed(f"{GIT} config --global user.email 'nixos@localhost'")
server.succeed(f"{GIT} config --global user.name 'NixOS test'")
server.succeed(f"{GIT} init /var/lib/git/test")
server.succeed("cp /var/lib/git/flake.nix /var/lib/git/flake.lock /var/lib/git/apps.nix /var/lib/git/test/")
server.succeed("install -D -m 0644 /var/lib/git/train.yaml /var/lib/git/test/secrets/train.yaml")
server.succeed("chmod u+w /var/lib/git/test/*")
server.succeed(f"sed -i 's#\\[hash\\]#{NIXPKGS_HASH}#g' /var/lib/git/test/flake.lock")
server.succeed("echo 'exec test repository' > /var/lib/git/test/README")
server.succeed(f"{GIT} -C /var/lib/git/test add flake.nix flake.lock apps.nix secrets/train.yaml README")
server.succeed(f"{GIT} -C /var/lib/git/test commit -m 'exec apps'")
server.succeed("chown -R git:git /var/lib/git/test")

banner("Phase 5: the evaluation records every exec job")
eval_id = wait_for(
    "the task starts an evaluation",
    lambda: api("GET", "tasks/project/task").get("last_evaluation"),
)


def recorded_jobs(evaluation, names):
    status = api("GET", f"evals/{evaluation}")["status"]
    assert status not in ("Failed", "Aborted"), f"evaluation {evaluation} ended {status}"
    jobs = api("GET", f"evals/{evaluation}/exec")
    by_name = {entry["entry_point"].rsplit(".", 1)[-1]: entry["id"] for entry in jobs}
    return by_name if set(names) <= by_name.keys() else None


JOBS = wait_for("the evaluation records every exec job", lambda: recorded_jobs(eval_id, NAMES), timeout=900)
pairs = [entry for entry in api("GET", f"evals/{eval_id}/exec") if entry["entry_point"] == "apps.x86_64-linux.pair"]
assert [entry["nodes"] for entry in pairs] == [2], f"the two-node job is not listed once: {json.dumps(pairs)}"

banner("Phase 6: upcoming exec jobs raise the warm target up to maxPreWarm")
stopper.wait_until_succeeds(
    "journalctl -u gradient-worker --no-pager | grep 'warm guest target changed' | grep -q 'target=2'",
    timeout=300,
)

banner("Phase 7: a networked job resolves a name and reaches the server")
fetched = wait_status("fetch", "completed")
assert end_reasons(fetched) == ["completed"], json.dumps(fetched)
output = job_log("fetch")
assert "served to the guest" in output, f"fetch did not reach the server:\n{output}"
assert f"job={JOBS['fetch']} attempt=1" in output, f"fetch saw no job context:\n{output}"

banner("Phase 7b: a job reports a phase, uploads outputs and builds with its job token")
produced = wait_status("outputs", "completed")
assert end_reasons(produced) == ["completed"], json.dumps(produced)
assert [phase["name"] for phase in produced["phases"]] == ["epoch-1"], json.dumps(produced)
assert len(produced["tokens"]) == 1 and produced["tokens"][0]["revoked"], json.dumps(produced)
assert "GRJ_" not in json.dumps(produced), "the job page shows a token value"
assert len(produced["started_evaluations"]) == 1, json.dumps(produced)
assert "::gradient-phase:: epoch-1" in job_log("outputs"), job_log("outputs")

outputs = {entry["path"]: entry for entry in api("GET", f"exec/{JOBS['outputs']}/outputs")}
assert outputs.keys() == {"model/w.bin", "gradient/tree.patch"}, json.dumps(outputs)
assert outputs["model/w.bin"]["size"] == OUTPUT_BYTES, json.dumps(outputs)

status, headers = download(f"exec/{JOBS['outputs']}/outputs/model/w.bin", "/tmp/w.part", "-H 'Range: bytes=0-9'")
assert status == "206", f"range download answered {status}:\n{headers}"
assert f"bytes 0-9/{OUTPUT_BYTES}" in headers, headers
assert server.succeed("stat -c %s /tmp/w.part").strip() == "10"

status, headers = download(f"exec/{JOBS['outputs']}/outputs/gradient/tree.patch", "/tmp/tree.patch")
assert status == "200", f"tree patch download answered {status}:\n{headers}"
patch = server.succeed("cat /tmp/tree.patch")
assert "+++ b/README" in patch and "+++ b/new.txt" in patch, patch
assert "w.bin" not in patch, patch

banner("Phase 7c: a two-node job runs on both workers and node 1 reaches the master")
pair = wait_status("pair", "completed")
pair_nodes = pair["node_jobs"]
assert [node["node"] for node in pair_nodes] == [0, 1], json.dumps(pair)
assert all(node["status"] == "completed" for node in pair_nodes), json.dumps(pair)
assert pair_nodes[0]["worker"] and pair_nodes[0]["worker"] != pair_nodes[1]["worker"], json.dumps(pair)
master_port = pair_nodes[0]["master_port"]
assert master_port and pair_nodes[1]["master_port"] == master_port, json.dumps(pair)
master, peer = pair_nodes[0]["guest_address"], pair_nodes[1]["guest_address"]
assert {subnet_of(master), subnet_of(peer)} == set(GUEST_SUBNETS), json.dumps(pair)

master_log = job_log("pair", node=0)
assert "hello from node 1 of 2" in master_log, f"node 0 heard nothing from node 1:\n{master_log}"
peer_lines = job_log("pair", node=1).splitlines()
assert f"master {master}:{master_port}" in peer_lines, f"node 1 got no master placeholders:\n{peer_lines}"
peers_line = next((line for line in peer_lines if line.startswith("peers ")), None)
assert peers_line, f"node 1 printed no GRADIENT_PEERS:\n{peer_lines}"
peers = json.loads(peers_line.removeprefix("peers "))
assert [(p["index"], p["address"], p["port"]) for p in peers] == [(0, master, master_port), (1, peer, master_port)], peers_line

cluster = sql(
    "SELECT cj.status, count(ca.id) FILTER (WHERE ca.started_at IS NOT NULL) FROM cluster_job cj "
    "LEFT JOIN cluster_attempt ca ON ca.cluster_job = cj.id "
    "WHERE cj.id IN (SELECT cluster_job FROM cluster_member WHERE exec_job IS NOT NULL) GROUP BY cj.id"
)
assert cluster == "2|1", f"expected one completed cluster job with one started attempt, got {cluster!r}"

banner("Phase 7d: a job boots cold with its own CPUs, memory and /work disk")
sized = wait_status("sized", "completed")
assert sized["guest"] == {"cpus": 1, "memory_mb": 768, "disk_mb": 256}, json.dumps(sized)
report = dict(line.split(None, 1) for line in job_log("sized").splitlines() if line.startswith(("cpus ", "memory_kb ", "work_mb ")))
assert report.get("cpus") == "1", f"sized guest CPUs: {report}"
assert 700 <= int(report["memory_kb"]) // 1024 <= 768, f"sized guest memory: {report}"
assert 200 <= int(report["work_mb"]) <= 256, f"sized guest /work disk: {report}"

banner("Phase 7e: a guest size no worker can hold waits with that size")
def unmet_guest(name):
    reason = job(name).get("waiting_reason") or {}
    return reason.get("unmet", {}).get("guest")


unmet = wait_for("toolarge waits for its guest size", lambda: unmet_guest("toolarge"))
assert unmet == {"cpus": 64, "memory_mb": 0, "disk_mb": 0}, f"toolarge waits for {unmet}"
assert job("toolarge")["attempts"] == [], json.dumps(job("toolarge"))
api("POST", f"exec/{JOBS['toolarge']}/abort")
wait_status("toolarge", "aborted", timeout=120)

banner("Phase 8: a silent program times out")
silent = wait_status("silent", "failed")
assert end_reasons(silent) == ["timeout"], json.dumps(silent)
assert "program produced no output for 5s (maxSilent exceeded)" in job_log("silent"), job_log("silent")

banner("Phase 9: an aborted job stops")
wait_running("abort", 3)
api("POST", f"exec/{JOBS['abort']}/abort")
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
assert "attempt 2 exits with 3" in job_log("fail", attempt=2), job_log("fail", attempt=2)

banner("Phase 12: a worker keeping its guests resumes the job, a restarted agent loses its job")
wait_running("long-keep", 5)
wait_running("lost", 5)
keep_guest = guest_of_job(keeper, JOBS["long-keep"])
lost_guest = guest_of_job(keeper, JOBS["lost"])
assert keep_guest and lost_guest, f"guests of the kept jobs: {keep_guest!r} {lost_guest!r}"
pid = main_pid(keeper, keep_guest)
keeper.succeed("systemctl stop gradient-worker.service")
restart = keeper.succeed(
    f"printf 'CONNECT {RESTART_AGENT_PORT}\\n' | timeout 90 socat STDIO,ignoreeof UNIX-CONNECT:{GUESTS}/{lost_guest}/vsock.sock")
assert "restarted" in restart, f"the lost guest did not restart its agent: {restart!r}"
keeper.succeed("systemctl start gradient-worker.service")
keeper.wait_until_succeeds(
    "journalctl -u gradient-worker --no-pager | grep -q 'resuming an exec job in its guest'", timeout=60)
assert main_pid(keeper, keep_guest) == pid, "the kept guest unit changed its main process"

kept = wait_status("long-keep", "completed")
assert end_reasons(kept) == ["completed"], json.dumps(kept)
lines = job_log("long-keep").splitlines()
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

banner("Phase 14: SSH reaches the jobs through the gateway")
server.wait_for_open_port(2222)
public_key = server.succeed(
    "mkdir -p /root/.ssh && ssh-keygen -q -t ed25519 -N '' -f /root/.ssh/gradient && cat /root/.ssh/gradient.pub"
).strip()
api("POST", "user/ssh-keys", {"name": "vm", "public_key": public_key})

interactive = wait_started("interactive")
job_login = f"project/{JOBS['interactive']}@localhost"
assert interactive["ssh_command"].startswith(f"ssh -p 2222 project/{JOBS['interactive']}@"), json.dumps(interactive)

new_shell = "printf 'echo gateway-$((6*7)); whoami; pwd\\nexit\\n' | {ssh} -T '{login}' shell"
for login in [job_login, "project/task@localhost"]:
    lines = server.succeed(new_shell.format(ssh=SSH, login=login)).splitlines()
    assert lines[-3:] == ["gateway-42", "gradient", "/work"], f"{login}: {lines}"

forward = f"{SSH} -o ExitOnForwardFailure=yes -N -f -L {{local}}:127.0.0.1:{{port}} '{job_login}' >/dev/null 2>&1"
server.succeed(forward.format(local=18080, port=8080))
served = server.succeed("curl -fsS --max-time 30 http://127.0.0.1:18080/flake.nix")
assert "apps" in served, served
server.succeed(forward.format(local=19090, port=9090))
server.fail("curl -fsS --max-time 5 http://127.0.0.1:19090/")
server.succeed("pkill -x ssh")

refused = server.fail(f"{SSH} '{job_login}' true 2>&1")
assert "only the command shell is supported" in refused, refused

api("POST", f"exec/{JOBS['interactive']}/abort")
assert end_reasons(wait_status("interactive", "aborted", timeout=120)) == ["aborted"]

wait_started("terminal")
attached = server.succeed(
    f"printf 'echo attached-$((6*7))\\nexit\\n' | {SSH} -T 'project/{JOBS['terminal']}@localhost'")
assert "attached-42" in attached, attached
terminal = wait_status("terminal", "completed", timeout=120)
assert end_reasons(terminal) == ["completed"], json.dumps(terminal)
assert "attached-42" in job_log("terminal"), job_log("terminal")

banner("Phase 15: secrets reach the job masked, a name in two sources fails before dispatch")
recipient = api("GET", "projects/project/sops-recipient")
assert recipient["age"] == AGE_RECIPIENT, json.dumps(recipient)

wait_status("secrets", "completed")
output = job_log("secrets")
expected = {"[secret:api-token]": 1, "[secret:wandb-token]": 2, "[secret:lab-key]": 1}
counts = {mask: output.count(mask) for mask in expected}
assert counts == expected, f"masks {counts} in the secrets log:\n{output}"
for value in ["api-token-value-1", "s3cr3t-value", "lab-value-123"]:
    assert value not in output, f"{value} left the guest unmasked:\n{output}"

clash = wait_status("clash", "failed")
assert clash["attempts"] == [], json.dumps(clash)
expected_message = f'secret "clash-key" found in two sources: project secrets, worker sops of {STOPPER_ID}'
assert clash["message"] == expected_message, json.dumps(clash)

banner("Phase 16: the workers settle on one paused warm guest")
for node in WORKERS:
    wait_for(f"{node.name} settles on one paused warm guest", lambda: one_paused_guest(node))
status, _ = keeper.execute("journalctl -u gradient-worker --no-pager | grep -q 'warm guest target changed'")
assert status != 0, "keeper with maxPreWarm = 0 changed its warm target"

banner("Phase 17: push the custom images into a flake directory of a second repository")
warm_guests = active_guests(stopper)
server.succeed(
    "nix --extra-experimental-features nix-command copy "
    "--to 'file:///srv/upstream?secret-key=/etc/gradient/secrets/upstream_key' "
    f"--no-check-sigs {TRAINER_INPUTS}"
)
server.succeed("chown -R nginx:nginx /srv/upstream")
server.succeed(f"{GIT} init /var/lib/git/images")
server.succeed("install -D -m 0644 /var/lib/git/images.nix /var/lib/git/images/nix/flake.nix")
server.succeed("install -m 0644 -t /var/lib/git/images/nix /var/lib/git/apps.nix /var/lib/git/guest.nix /var/lib/git/trainer.nix")
server.succeed("nix --extra-experimental-features 'nix-command flakes' flake lock path:/var/lib/git/images/nix")
server.succeed(f"{GIT} -C /var/lib/git/images add nix")
server.succeed(f"{GIT} -C /var/lib/git/images commit -m 'custom images'")
server.succeed("chown -R git:git /var/lib/git/images")

images_eval = wait_for(
    "the images task starts an evaluation",
    lambda: api("GET", "tasks/project/images").get("last_evaluation"),
)
JOBS.update(wait_for(
    "the evaluation records the custom image jobs", lambda: recorded_jobs(images_eval, IMAGE_NAMES), timeout=900))

banner("Phase 18: a custom image boots cold on the worker that allows custom images")
trained = wait_status("trainer", "completed", timeout=1200)
assert end_reasons(trained) == ["completed"], json.dumps(trained)
assert trained["attempts"][0]["worker"] == STOPPER_ID, json.dumps(trained)
output = job_log("trainer")
assert "trainer" in output.splitlines(), f"the job did not run in the custom image:\n{output}"
assert "cwd /work/nix" in output, f"the program did not start in the flake directory:\n{output}"
assert set(warm_guests) <= set(active_guests(stopper)), "the custom image job took the warm guest"

banner("Phase 19: a custom image without the guest agent fails with a hint")
missing = wait_status("noagent", "failed", timeout=900)
assert end_reasons(missing) == ["failed"], json.dumps(missing)
messages = [missing["message"]] + [attempt["message"] for attempt in missing["attempts"]]
assert any(NO_GUEST_AGENT in (message or "") for message in messages), json.dumps(missing)

wait_for(
    "stopper tears the custom guests down and keeps its warm guest",
    lambda: active_guests(stopper) == warm_guests and one_paused_guest(stopper),
)
