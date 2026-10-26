# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

# Evaluates a large nixpkgs-based flake once per worker count with every cache wiped,
# then a single-attribute flake standalone and with every worker up, and fails when a
# target of #568 is missed. The summary is published before that verdict.

EVALUATED = {"Building", "Waiting", "Completed"}
EVAL_TIMEOUT = 3600
CPUS = sorted(os.sched_getaffinity(0))


def assert_enough_cpus():
    needed = len(WORKER_NODES) * WORKER_CORES + SERVER_CORES
    assert len(CPUS) >= needed, (
        f"the multinode evalbench needs {needed} CPUs for its VMs, this builder has {len(CPUS)}"
    )


def pin(machine, cpus):
    process = machine.process
    assert process is not None, f"{machine.name} has no QEMU process"
    for thread in pathlib.Path(f"/proc/{process.pid}/task").iterdir():
        os.sched_setaffinity(int(thread.name), cpus)


def pin_vms():
    pin(server, CPUS[:SERVER_CORES])
    for index, node in enumerate(WORKER_NODES):
        first = SERVER_CORES + WORKER_CORES * index
        pin(node, CPUS[first:first + WORKER_CORES])


def wait_links_shaped():
    for node in WORKER_NODES:
        try:
            node.wait_for_unit("shape-links.service")
        except Exception:
            print(node.execute("journalctl -u shape-links.service --no-pager -b")[1])
            raise


def assert_shaped_links():
    for node in WORKER_NODES:
        reply = node.succeed("ping -c 3 server")
        rtt = re.search(r"= [\d.]+/([\d.]+)/", reply)
        assert rtt, f"{node.name}: no round trip in {reply}"
        average_ms = float(rtt.group(1))
        assert average_ms >= 2 * DELAY_MS, f"{node.name}: {average_ms} ms round trip to the server"


def seed(repo):
    path = f"/var/lib/git/{repo}"
    server.succeed(f"{GIT} init {path} && cp /var/lib/git/{repo}.nix {path}/flake.nix"
                   f" && cp /var/lib/git/{repo}.lock {path}/flake.lock")
    server.succeed(f"sed -i 's#\\[hash\\]#{NIXPKGS_HASH}#g' {path}/flake.lock")
    server.succeed(f"{GIT} -C {path} add flake.nix flake.lock && {GIT} -C {path} commit -m 'Initial commit'")
    server.succeed(f"chown git:git -R {path}")


def bump(repo):
    path = f"/var/lib/git/{repo}"
    server.succeed(f"echo '# bumped {time.time_ns()}' >> {path}/flake.nix")
    server.succeed(f"{GIT} -C {path} commit -am 'Bump' && chown git:git -R {path}")


def reset_workers():
    epoch = server.succeed("date +%s").strip()
    for node in WORKER_NODES:
        node.systemctl("stop gradient-worker.service")
        node.succeed("rm -rf /var/lib/gradient-worker/eval-cache /var/lib/gradient-worker/www/.cache/nix")
        node.succeed("nix-collect-garbage >&2")
        node.systemctl("start gradient-worker.service")
    for node in WORKER_NODES:
        node.wait_until_succeeds(
            f"journalctl -u gradient-worker --since=@{epoch} --no-pager | grep -q 'handshake successful'",
            timeout=180,
        )


def connected_workers(token):
    workers = api("GET", "projects/project/workers", token)
    return sum(1 for worker in workers if worker.get("live") is not None)


def use_workers(token, n):
    for node in WORKER_NODES[n:]:
        node.systemctl("stop gradient-worker.service")
    deadline = time.monotonic() + 180
    while connected_workers(token) != n:
        assert time.monotonic() < deadline, f"the server never listed exactly {n} connected workers"
        time.sleep(1)


def evaluation_rows(query, eval_id):
    return json.loads(psql(f"SELECT coalesce(json_agg(t), '[]') FROM ({query.format(eval_id=eval_id)}) t;"))


def eval_phases(eval_id):
    return evaluation_rows(
        "SELECT CASE p.phase WHEN 2 THEN 'eval_flake' WHEN 3 THEN 'eval_derivations' ELSE 'other' END AS phase,"
        " p.start_ms, p.end_ms,"
        " (extract(epoch FROM dj.dispatched_at - min(dj.dispatched_at) OVER ()) * 1000)::bigint AS offset_ms"
        " FROM dispatched_job_phase p JOIN dispatched_job dj ON dj.id = p.dispatched_job"
        " WHERE dj.evaluation_id = '{eval_id}' AND dj.kind = 0",
        eval_id,
    )


def measure(token, task):
    banner(f"evaluate {task} on {connected_workers(token)} workers")
    psql("SELECT pg_stat_statements_reset();")
    eval_id = api("POST", f"tasks/project/{task}/evaluate", token, body={})
    status = wait_for_status(token, eval_id, EVALUATED, timeout=EVAL_TIMEOUT)
    statements = int(psql(
        "SELECT coalesce(sum(s.calls), 0) FROM pg_stat_statements s"
        " JOIN pg_roles r ON r.oid = s.userid WHERE r.rolname = 'gradient'"
    ))
    shards = int(psql(f"SELECT count(*) FROM eval_shard WHERE evaluation = '{eval_id}' AND state IN (2, 3)"))
    jobs = int(psql(
        "SELECT count(*) FROM dispatched_job dj"
        f" WHERE dj.evaluation_id = '{eval_id}' AND dj.kind = 0"
        " AND EXISTS (SELECT 1 FROM dispatched_job_phase p WHERE p.dispatched_job = dj.id AND p.phase IN (2, 3))"
    ))
    result = {
        "eval_id": eval_id,
        "wall_ms": report.wall_ms(eval_phases(eval_id)),
        "statements": statements,
        "shards": shards,
        "jobs": jobs,
    }
    if status != "Completed":
        api("POST", f"evals/{eval_id}", token, body={"method": "abort"})
    return result


def write_summary(runs, small):
    out = pathlib.Path(server.out_dir)
    (out / "summary.json").write_text(json.dumps({"runs": runs, "small": small}, indent=2))
    text = report.render(runs, small) if 1 in runs else ""
    (out / "summary.txt").write_text(text)
    print(text)


def measure_large(token):
    runs = {}
    for n in range(1, len(WORKER_NODES) + 1):
        bump("large")
        reset_workers()
        use_workers(token, n)
        runs[n] = measure(token, "large")
        write_summary(runs, {})

    return runs


def measure_small(token, runs):
    small = {}
    for label, n in (("standalone", 1), ("multinode", len(WORKER_NODES))):
        bump("small")
        reset_workers()
        use_workers(token, n)
        small[label] = measure(token, "small")
        write_summary(runs, small)

    return small


assert_enough_cpus()
start_all()
pin_vms()
banner("bring services up")
server.wait_for_unit("gradient-server.service")
server.wait_for_unit("nginx.service")
wait_links_shaped()
wait_workers_ready()
assert_shaped_links()

banner("seed the bench repositories")
server.succeed(f"{GIT} config --global --add safe.directory '*'")
server.succeed(f"{GIT} config --global init.defaultBranch main")
server.succeed(f"{GIT} config --global user.email 'nixos@localhost' && {GIT} config --global user.name 'NixOS test'")
seed("large")
seed("small")
psql("CREATE EXTENSION IF NOT EXISTS pg_stat_statements;")
token = login()

banner("record the costs of the large flake")
use_workers(token, 1)
measure(token, "large")

runs = measure_large(token)
small = measure_small(token, runs)
out = pathlib.Path(server.out_dir)
publish(out, name="evalbench-multinode")
failed = report.verdicts(runs, small)
assert not failed, "missed #568 targets:\n" + "\n".join(failed)
