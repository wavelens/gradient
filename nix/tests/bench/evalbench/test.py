# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

# Evaluates the e2e hello flake cold and warm, first with only the cheap captures
# (spans, pcaps, Postgres statistics) whose timings the summary reports, then again
# under perf and strace, whose overhead would distort those timings.

import subprocess

RUNS = [
    ("cold-clean", True, False),
    ("warm-clean", False, False),
    ("cold-instrumented", True, True),
    ("warm-instrumented", False, True),
]
EVALUATED = {"Building", "Waiting", "Completed"}
FAILED = {"Failed", "Aborted"}

worker = WORKER_NODES[0]


def banner(msg):
    print(f"\n=== {msg} ===")


def psql_command(query, database="gradient"):
    server.succeed(f"cat > /tmp/q.sql <<'EOF'\n{query}\nEOF")
    return f"su postgres -c 'psql -v ON_ERROR_STOP=1 -d {database} -At -f /tmp/q.sql'"


def psql(query, database="gradient"):
    return server.succeed(psql_command(query, database)).strip()


def dump_json(query, path):
    server.succeed(psql_command(f"SELECT coalesce(json_agg(t), '[]') FROM ({query}) t;") + f" > {path}")


def api(method, path, token=None, body=None):
    cmd = f"curl -sf -X {method} {API}/{path} -H 'Content-Type: application/json'"
    if token:
        cmd += f" -H 'Authorization: Bearer {token}'"
    if body is not None:
        cmd += f" -d '{json.dumps(body)}'"
    reply = json.loads(server.succeed(cmd))
    assert reply.get("error") is False, f"{method} {path}: {reply}"
    return reply["message"]


def login():
    return api("POST", "auth/basic/login", body={"loginname": "admin", "password": "admin_password"})


def wait_workers_since(epoch):
    for node in WORKER_NODES:
        node.wait_until_succeeds(
            f"journalctl -u gradient-worker --since=@{epoch} --no-pager | grep -q 'handshake successful'",
            timeout=180,
        )


def reset_to_cold():
    epoch = server.succeed("date +%s").strip()
    for node in WORKER_NODES:
        node.systemctl("stop gradient-worker.service")
    server.systemctl("stop gradient-server.service")
    psql("DROP DATABASE gradient WITH (FORCE); CREATE DATABASE gradient OWNER gradient;",
         database="postgres")
    psql("CREATE EXTENSION IF NOT EXISTS pg_stat_statements;")
    server.systemctl("start gradient-server.service")
    server.wait_for_unit("gradient-server.service")
    for node in WORKER_NODES:
        node.succeed("rm -rf /var/lib/gradient-worker/eval-cache /var/lib/gradient-worker/www/.cache/nix")
        node.systemctl("start gradient-worker.service")
    wait_workers_since(epoch)


def wait_for_status(token, eval_id, wanted, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        status = api("GET", f"evals/{eval_id}", token)["status"]
        if status in FAILED:
            raise Exception(f"evaluation {eval_id} {status}:\n"
                            + server.succeed("journalctl -u gradient-server --no-pager -n 100"))
        if status in wanted:
            return status
        time.sleep(1)
    raise Exception(f"evaluation {eval_id} did not reach {wanted} within {timeout} s")


def dump_postgres(run, eval_id):
    pg = f"{BENCH}/{run}/pg"
    dump_json(
        "SELECT s.calls, round(s.total_exec_time::numeric, 3) AS total_ms,"
        " round(s.mean_exec_time::numeric, 3) AS mean_ms, s.rows,"
        " s.shared_blks_hit, s.shared_blks_read, s.query"
        " FROM pg_stat_statements s JOIN pg_roles r ON r.oid = s.userid"
        " WHERE r.rolname = 'gradient' ORDER BY s.total_exec_time DESC LIMIT 100",
        f"{pg}/pg_stat_statements.json",
    )
    dump_json(
        "SELECT dj.kind, p.seq, p.parent_seq, p.phase, p.start_ms, p.end_ms"
        " FROM dispatched_job_phase p JOIN dispatched_job dj ON dj.id = p.dispatched_job"
        f" WHERE dj.evaluation_id = '{eval_id}' ORDER BY dj.dispatched_at, p.seq",
        f"{pg}/job_phases.json",
    )


def evaluate(run, cold, instrumented):
    banner(f"{run}: {'cold' if cold else 'warm'}, {'instrumented' if instrumented else 'clean'}")
    if cold:
        reset_to_cold()

    token = login()
    prepare(run)
    psql("SELECT pg_stat_statements_reset();")
    set_auto_explain(instrumented)
    log = postgres_log()
    log_from = log_size(log)
    if instrumented:
        start_profilers(run)
    else:
        start_pcaps(run)

    started = time.monotonic()
    eval_id = api("POST", "tasks/project/task/evaluate", token, body={})
    wait_for_status(token, eval_id, EVALUATED, timeout=900)
    evaluated_s = time.monotonic() - started

    if instrumented:
        stop_profilers(run)
    else:
        stop_pcaps()

    set_auto_explain(False)
    dump_postgres(run, eval_id)
    collect_traces(run)
    if instrumented:
        server.succeed(f"tail -c +{log_from + 1} {log} > {BENCH}/{run}/pg/auto_explain.log")

    wait_for_status(token, eval_id, {"Completed"}, timeout=1200)
    dump_json(f"SELECT * FROM evaluation_metric WHERE evaluation = '{eval_id}'",
              f"{BENCH}/{run}/evaluation_metric.json")
    server.succeed(
        f"echo '{json.dumps({'eval_id': eval_id, 'cold': cold, 'instrumented': instrumented, 'evaluated_s': round(evaluated_s, 1)})}'"
        f" > {BENCH}/{run}/run.json"
    )
    return copy_out(run)


def assert_captured(host, instrumented):
    expected = ["run.json", "pg/pg_stat_statements.json", "evaluation_metric.json"]
    expected += (["perf.data", "flame.svg", "worker/perf.data", "worker/flame.svg", "pg/auto_explain.log"]
                 if instrumented else ["proto.pcap", "pg.pcap"])
    missing = [name for name in expected if not (host / name).exists()]
    processes = {p.name.split("-")[0] for p in (host / "trace").glob("*.jsonl") if p.stat().st_size}
    missing += [f"trace/{p}-*.jsonl" for p in ("server", "worker", "eval") if p not in processes]
    if instrumented and not any((host / "worker" / "strace").iterdir()):
        missing.append("worker/strace/*")
    assert not missing, f"{host.name} lacks {missing}"


start_all()
banner("bring services up")
server.wait_for_unit("gradient-server.service")
wait_workers_ready()

banner("seed the e2e repository")
server.succeed(f"{GIT} config --global --add safe.directory '*'")
server.succeed(f"{GIT} config --global init.defaultBranch main")
server.succeed(f"{GIT} config --global user.email 'nixos@localhost' && {GIT} config --global user.name 'NixOS test'")
server.succeed(f"{GIT} init /var/lib/git/test && cp /var/lib/git/{{,test/}}flake.nix && cp /var/lib/git/{{,test/}}flake.lock")
server.succeed(f"sed -i 's#\\[nixpkgs\\]#{NIXPKGS}#g' /var/lib/git/test/flake.nix /var/lib/git/test/flake.lock")
server.succeed(f"sed -i 's#\\[hash\\]#{NIXPKGS_HASH}#g' /var/lib/git/test/flake.lock")
server.succeed(f"{GIT} -C /var/lib/git/test add flake.nix flake.lock && {GIT} -C /var/lib/git/test commit -m 'Initial commit'")
server.succeed("chown git:git -R /var/lib/git/test")

for run, cold, instrumented in RUNS:
    assert_captured(evaluate(run, cold, instrumented), instrumented)

out = pathlib.Path(server.out_dir)
for run, _, instrumented in RUNS:
    if instrumented:
        summarize_run(out / run)
summarize(out, [run for run, _, instrumented in RUNS if not instrumented])
subprocess.run([INSPECTOR, str(out), "-o", str(out / "report")], check=True)
publish(out, out / "report" / "index.html")
print((out / "summary.txt").read_text())
