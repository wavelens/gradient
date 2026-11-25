# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

# Eval caches are wiped before every run, the worker stores keep nixpkgs.

from datetime import timedelta

EVALUATED = {"Building", "Waiting", "Completed"}
EVALUATION_STATUS_NAME_TO_DATABASE_VALUE = {"Building": 3, "Waiting": 4, "Completed": 5}
EVAL_TIMEOUT = 900
# Eval cache segment uploads add one compaction delivery to the queue, due after 15 minutes. Waiting for that delivery can leave each phase idle. Leased rows move next_attempt_at forward, so a filter on due time can skip deliveries that are still in progress.
EVAL_CACHE_COMPACT_PENDING_DELIVERY_KIND = 7
POSTGRES_LOG = "/var/lib/postgresql/18/log/postgresql.log"
server_trace_directory = "/var/lib/gradient/trace"
worker_trace_directory = "/var/lib/gradient-worker/trace"
CPUS = sorted(os.sched_getaffinity(0))
WORKER_COUNTS_TO_MEASURE = [int(n) for n in os.environ.get(
    "MULTINODE_EVAL_WORKER_COUNTS", ",".join(str(n) for n in range(1, len(WORKER_NODES) + 1))
).split(",")]
MEASURE_SMALL_FLAKE = os.environ.get("MEASURE_SMALL_FLAKE_BENCHMARK", "1") != "0"
assert all(1 <= n <= len(WORKER_NODES) for n in WORKER_COUNTS_TO_MEASURE), (
    f"MULTINODE_EVAL_WORKER_COUNTS lists worker counts outside the range 1 to {len(WORKER_NODES)}: {WORKER_COUNTS_TO_MEASURE}"
)
JOB_PHASE_NUMBER_TO_NAME = {
    0: "fetch", 1: "push_inputs", 2: "eval_flake", 3: "eval_derivations", 4: "eval_cache_pull",
    5: "eval_cache_push", 6: "known_derivations_wait", 7: "drv_closure_push", 8: "prefetch",
    9: "substitute_passthrough", 10: "build", 11: "compress", 12: "nar_push", 13: "cache_query_wait",
    14: "substitute_fetch", 15: "download", 16: "nar_fetch", 17: "nar_import", 18: "upload_wait",
}
PHASE_NUMBER_TO_PHASE_NAME_SQL_CASE = (
    "CASE p.phase " + " ".join(f"WHEN {n} THEN '{name}'" for n, name in JOB_PHASE_NUMBER_TO_NAME.items())
    + " ELSE 'unknown_' || p.phase END"
)


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
    siblings = {}
    for cpu in CPUS:
        topology = pathlib.Path(f"/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list")
        if topology.exists():
            siblings[cpu] = topology.read_text().strip()
    print(f"builder CPUs {CPUS} of {os.cpu_count()}, load {os.getloadavg()}, siblings {siblings}")


def cpu_seconds(node):
    fields = node.succeed("head -1 /proc/stat").split()
    return {name: int(value) / 100 for name, value in zip(["user", "nice", "system", "idle", "iowait", "irq", "softirq", "steal"], fields[1:9])}


def save_vm_load(label, before):
    loads = {}
    for node in [server] + WORKER_NODES:
        delta = {name: round(value - before[node.name][name]) for name, value in cpu_seconds(node).items()}
        loadavg = node.succeed("cat /proc/loadavg").strip()
        print(f"cpu seconds of {node.name} during {label}: {delta}, load {loadavg}")
        loads[node.name] = {**delta, "load": float(loadavg.split()[0])}
    return loads


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
    if server.execute(f"test -d {path}/.git")[0] == 0:
        return
    server.succeed(f"{GIT} init {path} && cp /var/lib/git/{repo}.nix {path}/flake.nix"
                   f" && cp /var/lib/git/{repo}.lock {path}/flake.lock")
    server.succeed(f"sed -i 's#\\[hash\\]#{NIXPKGS_HASH}#g' {path}/flake.lock")
    server.succeed(f"{GIT} -C {path} add flake.nix flake.lock && {GIT} -C {path} commit -m 'Initial commit'")
    server.succeed(f"chown git:git -R {path}")


def bump(repo):
    path = f"/var/lib/git/{repo}"
    server.succeed(f"echo '# bumped {time.time_ns()}' >> {path}/flake.nix")
    server.succeed(f"{GIT} -C {path} commit -am 'Bump' && chown git:git -R {path}")


def keep_nixpkgs():
    unpack = "nix-prefetch-url --unpack --name source --print-path http://server/nixpkgs.tar.gz"
    worker_vms_without_nixpkgs_gc_root = [
        node for node in WORKER_NODES if node.execute("test -e /nix/var/nix/gcroots/nixpkgs")[0] != 0
    ]
    for node in worker_vms_without_nixpkgs_gc_root:
        node.succeed("rm -f /tmp/nixpkgs.done")
        node.execute(f"({unpack} > /tmp/nixpkgs.out 2> /tmp/nixpkgs.log; touch /tmp/nixpkgs.done) > /dev/null 2>&1 &")
    for node in worker_vms_without_nixpkgs_gc_root:
        node.wait_until_succeeds("test -e /tmp/nixpkgs.done", timeout=timedelta(seconds=600))
        path = node.succeed("tail -1 /tmp/nixpkgs.out").strip()
        assert path.startswith("/nix/store/"), f"{node.name} did not unpack nixpkgs:\n" + node.succeed("cat /tmp/nixpkgs.log")
        info = json.loads(node.succeed(f"nix --extra-experimental-features nix-command path-info --json {path}"))
        nar_hash = (info[0] if isinstance(info, list) else next(iter(info.values())))["narHash"]
        assert nar_hash == NIXPKGS_HASH, f"{node.name} unpacked {path} with {nar_hash} instead of {NIXPKGS_HASH}"
        node.succeed(f"nix-store --add-root /nix/var/nix/gcroots/nixpkgs -r {path}")


def reset_workers():
    for node in WORKER_NODES:
        node.systemctl("stop gradient-worker.service")
        node.succeed("rm -rf /var/lib/gradient-worker/eval-cache /var/lib/gradient-worker/www/.cache/nix /tmp/gc.done")
        node.execute("(nix-collect-garbage > /tmp/gc.log 2>&1; touch /tmp/gc.done) > /dev/null 2>&1 &")
    for node in WORKER_NODES:
        node.wait_until_succeeds("test -e /tmp/gc.done", timeout=timedelta(seconds=600))
        print(node.succeed("grep -v \"^deleting '\" /tmp/gc.log || true"))
        epoch = node.succeed("date +%s").strip()
        node.systemctl("start gradient-worker.service")
        node.wait_until_succeeds(
            f"journalctl -u gradient-worker --since=@{epoch} --no-pager | grep -q 'handshake successful'",
            timeout=timedelta(seconds=180),
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
        " right(dj.id::text, 8) AS job, p.start_ms, p.end_ms,"
        " (extract(epoch FROM dj.dispatched_at - min(dj.dispatched_at) OVER ()) * 1000)::bigint AS offset_ms"
        " FROM dispatched_job_phase p JOIN dispatched_job dj ON dj.id = p.dispatched_job"
        " WHERE dj.evaluation_id = '{eval_id}' AND dj.kind = 0",
        eval_id,
    )


def evaluation_request_to_first_non_evaluating_status_ms(eval_id):
    left_evaluating = sorted(EVALUATION_STATUS_NAME_TO_DATABASE_VALUE[name] for name in EVALUATED)
    milliseconds = psql(
        "SELECT round(extract(epoch FROM coalesce("
        " (SELECT min(pe.at) FROM phase_event pe WHERE pe.subject_kind = 1 AND pe.subject_id = e.id"
        f" AND pe.phase IN ({', '.join(map(str, left_evaluating))})),"
        " least(e.building_started_at, e.finished_at)) - e.created_at) * 1000)::bigint"
        f" FROM evaluation e WHERE e.id = '{eval_id}'"
    )
    assert milliseconds, f"evaluation {eval_id} has no recorded end of its evaluating statuses"
    return int(milliseconds)


def shard_timings(eval_id):
    return evaluation_rows(
        "SELECT s.seq, s.pattern, coalesce(cardinality(s.children), 0) AS children,"
        " round(s.predicted_ms) AS predicted_ms, s.state,"
        " CASE WHEN s.taken_job IS NULL THEN NULL WHEN split_part(s.taken_job, ':', 3) = '' THEN 'primary'"
        " ELSE split_part(s.taken_job, ':', 3) END AS member,"
        " round(extract(epoch FROM s.taken_at - first.at) * 1000) AS taken_ms,"
        " round(extract(epoch FROM s.finished_at - s.taken_at) * 1000) AS took_ms"
        " FROM eval_shard s, (SELECT min(dispatched_at) AS at FROM dispatched_job"
        " WHERE evaluation_id = '{eval_id}' AND kind = 0) first"
        " WHERE s.evaluation = '{eval_id}' ORDER BY s.taken_at, s.seq",
        eval_id,
    )


def save_job_phases(label, eval_id):
    text = psql(
        f"SELECT right(dj.id::text, 8) || ' ' || {PHASE_NUMBER_TO_PHASE_NAME_SQL_CASE} || ': ' || count(*) || ' x, '"
        " || sum(p.end_ms - p.start_ms) || ' ms, from +' || min(p.start_ms) || ' to +' || max(p.end_ms)"
        " FROM dispatched_job_phase p JOIN dispatched_job dj ON dj.id = p.dispatched_job"
        f" WHERE dj.evaluation_id = '{eval_id}' AND dj.kind = 0"
        " GROUP BY dj.id, p.phase ORDER BY dj.id, p.phase"
    )
    print(f"Job phases for {label}:\n{text}")


def save_shards(label, eval_id):
    text = psql(
        "SELECT s.seq || ' ' || s.pattern || ': ' || coalesce(cardinality(s.children), 0) || ' children, predicted '"
        " || round(s.predicted_ms) || ' ms, state ' || s.state || ', member '"
        " || CASE WHEN s.taken_job IS NULL THEN 'nobody' WHEN split_part(s.taken_job, ':', 3) = '' THEN 'primary'"
        " ELSE split_part(s.taken_job, ':', 3) END || ', taken +'"
        " || coalesce(round(extract(epoch FROM s.taken_at - first.at) * 1000)::text, '-') || ' ms, took '"
        " || coalesce(round(extract(epoch FROM s.finished_at - s.taken_at) * 1000)::text, '-') || ' ms'"
        " FROM eval_shard s, (SELECT min(dispatched_at) AS at FROM dispatched_job"
        f" WHERE evaluation_id = '{eval_id}' AND kind = 0) first"
        f" WHERE s.evaluation = '{eval_id}' ORDER BY s.taken_at, s.seq"
    )
    print(f"shards of {label} (queued 0, running 1, done 2, failed 3, split 4):\n{text}")


def statements_ranked_by(column):
    return psql(
        "SELECT round(max_exec_time) || ' ms max, ' || calls || ' calls, '"
        " || round(total_exec_time) || ' ms total: ' || left(regexp_replace(query, '\\s+', ' ', 'g'), 400)"
        f" FROM pg_stat_statements ORDER BY {column} DESC LIMIT 25"
    )


def heaviest_statements():
    return json.loads(psql(
        "SELECT coalesce(json_agg(t), '[]') FROM (SELECT left(regexp_replace(query, '\\s+', ' ', 'g'), 400) AS query,"
        " calls, round(total_exec_time) AS total_ms, round(max_exec_time) AS max_ms"
        " FROM pg_stat_statements ORDER BY total_exec_time DESC LIMIT 15) t;"
    ))


def save_slow_statements(label):
    slowest = statements_ranked_by("max_exec_time")
    heaviest = statements_ranked_by("total_exec_time")
    logged = server.succeed(f"cat {POSTGRES_LOG}")
    text = f"by max time:\n{slowest}\n\nby total time:\n{heaviest}\n\n{logged}"
    (pathlib.Path(server.out_dir) / f"slow-statements-{label}.txt").write_text(text)
    print(f"slow statements of {label}:\n{text}")
    return heaviest_statements()


def get_span_trace_file_sizes(node):
    directory = server_trace_directory if node is server else worker_trace_directory
    listing = node.succeed(f"stat -c '%s %n' {directory}/*.jsonl 2>/dev/null || true")
    return {path: int(size) for size, path in (line.split(" ", 1) for line in listing.splitlines())}


def copy_spans_after_snapshot(node, sizes, name):
    tails = "".join(f"tail -c +{sizes.get(path, 0) + 1} {path}; " for path in get_span_trace_file_sizes(node))
    node.succeed(f"{{ {tails}true; }} > /tmp/{name}")
    node.copy_from_machine(f"/tmp/{name}")
    path = pathlib.Path(node.out_dir) / name
    spans = [span for span in map(parsed_span, path.read_text().splitlines()) if span]
    path.write_text("\n".join(json.dumps(span) for span in spans))
    return spans


def format_top_span_table(spans):
    by_name = {}
    for span in spans:
        total, count, longest = by_name.get(span["name"], (0, 0, 0))
        by_name[span["name"]] = (total + span["dur_us"], count + 1, max(longest, span["dur_us"]))
    return "\n".join(
        f"{total // 1000} ms total, {count} calls, {longest // 1000} ms max: {name}"
        for name, (total, count, longest) in sorted(by_name.items(), key=lambda item: -item[1][0])[:25]
    )


SPAN_KEYS = {"name", "target", "ts_us", "dur_us", "fields"}


def parsed_span(line):
    try:
        span = json.loads(line) if line.startswith("{") else None
    except json.JSONDecodeError:
        return None
    return span if isinstance(span, dict) and SPAN_KEYS <= span.keys() else None


def save_slow_spans(label, sizes):
    spans = copy_spans_after_snapshot(server, sizes, f"spans-{label}.jsonl")
    first = min((span["ts_us"] for span in spans), default=0)
    timeline = "\n".join(
        f"+{(span['ts_us'] - first) // 1000:>7} ms {span['dur_us'] // 1000:>6} ms {span['name']} {json.dumps(span['fields'])}"
        for span in sorted(spans, key=lambda span: span["ts_us"])
        if span["dur_us"] >= 1_000_000 and span["target"].startswith(("gradient_graph", "gradient_db"))
    )
    print(f"Spans ordered by total time in {label}:\n{format_top_span_table(spans)}\n\nSpans over one second:\n{timeline}")
    return f"spans-{label}.jsonl"


def save_and_print_worker_vm_spans(label, nodes, sizes):
    worker_vm_name_to_spans_file_name = {}
    for node in nodes:
        name = f"spans-{label}-{node.name}.jsonl"
        spans = copy_spans_after_snapshot(node, sizes[node.name], name)
        print(f"Spans for {node.name} in {label} (by total time):\n{format_top_span_table(spans)}")
        worker_vm_name_to_spans_file_name[node.name] = name
    return worker_vm_name_to_spans_file_name


def has_warm_up_eval_cost_rows_for_task(task):
    return psql(
        "SELECT EXISTS (SELECT 1 FROM eval_set_cost s JOIN task t ON t.id = s.task"
        f" JOIN eval_subtree_cost c ON c.task = t.id WHERE t.name = '{task}');"
    ) == "t"


def sync_vm_disks():
    for node in [server] + WORKER_NODES:
        node.succeed("sync")


def save_eval_costs(label):
    costs = json.loads(psql(
        "SELECT coalesce(json_agg(c), '[]') FROM (SELECT array_to_string(s.path, '.') AS path,"
        " round(coalesce(s.startup_ms, 0)) AS startup_ms, s.samples, count(t.id) AS children,"
        " round(coalesce(sum(t.attrs * t.ms_per_attr), 0)) AS work_ms,"
        " round(coalesce(percentile_cont(0.5) WITHIN GROUP (ORDER BY t.attrs * t.ms_per_attr), 0)) AS median_ms,"
        " round(coalesce(max(t.attrs * t.ms_per_attr), 0)) AS max_ms"
        " FROM eval_set_cost s LEFT JOIN eval_subtree_cost t ON t.task = s.task"
        " AND cardinality(t.path) = cardinality(s.path) + 1 AND t.path[1:cardinality(s.path)] = s.path"
        " GROUP BY s.id, s.path, s.startup_ms, s.samples ORDER BY s.path) c;"
    ))
    text = "\n".join(
        f"{c['path']}: startup {c['startup_ms']} ms, {c['samples']} samples, {c['children']} children,"
        f" {c['work_ms']} ms work, median child {c['median_ms']} ms, max child {c['max_ms']} ms"
        for c in costs
    )
    print(f"learned costs after {label}:\n{text}")
    return costs


def active_statements():
    return psql(
        "SELECT to_char(now(), 'HH24:MI:SS') || ' ' || coalesce(wait_event_type || '/' || wait_event, 'running')"
        " || ' ' || to_char(now() - xact_start, 'MI:SS') || ' into its transaction: '"
        " || left(regexp_replace(query, '\\s+', ' ', 'g'), 200)"
        " FROM pg_stat_activity WHERE state = 'active' AND pid <> pg_backend_pid()"
    )


def wait_until_settled(eval_id):
    psql("ANALYZE build_job, derivation_build;")
    deadline = time.monotonic() + EVAL_TIMEOUT
    samples = []
    while int(psql(
        "SELECT count(*) FROM build_job bj JOIN derivation_build db ON db.id = bj.derivation_build"
        f" WHERE bj.evaluation = '{eval_id}' AND db.status IN (1, 2)"
    )):
        samples.append(active_statements())
        assert time.monotonic() < deadline, (
            f"evaluation {eval_id} still has queued or building shared builds after its abort\n"
            "active statements while waiting:\n" + "\n".join(samples) + "\n"
            + server.succeed("journalctl -u gradient-server --no-pager -n 50 | grep -v 'job failed' || true")
        )
        time.sleep(2)


def wait_until_delivered():
    deadline = time.monotonic() + EVAL_TIMEOUT
    while int(psql(
        "SELECT count(*) FROM pending_delivery WHERE delivered_at IS NULL AND failed_at IS NULL"
        f" AND kind <> {EVAL_CACHE_COMPACT_PENDING_DELIVERY_KIND}"
    )):
        assert time.monotonic() < deadline, "the delivery queue of the previous run never drained"
        time.sleep(2)


def measure(token, task):
    workers = connected_workers(token)
    banner(f"evaluate {task} on {workers} workers")
    wait_until_delivered()
    psql("SELECT pg_stat_statements_reset();")
    server.succeed(f": > {POSTGRES_LOG}")
    load_before = {node.name: cpu_seconds(node) for node in [server] + WORKER_NODES}
    span_sizes = {node.name: get_span_trace_file_sizes(node) for node in [server] + WORKER_NODES[:workers]}
    eval_id = api("POST", f"tasks/project/{task}/evaluate", token, body={})["id"]
    label = f"{task}-{workers}"
    captured = {}
    try:
        status = wait_for_status(token, eval_id, EVALUATED, timeout=EVAL_TIMEOUT, poll=2)
        wait_until_delivered()
    finally:
        captured["load"] = save_vm_load(label, load_before)
        captured["top_statements"] = save_slow_statements(label)
        captured["spans_file"] = save_slow_spans(label, span_sizes[server.name])
        captured["worker_vm_name_to_spans_file_name"] = save_and_print_worker_vm_spans(
            label, WORKER_NODES[:workers], span_sizes
        )
        captured["costs"] = save_eval_costs(label)
        save_job_phases(label, eval_id)
        save_shards(label, eval_id)
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
    phases = eval_phases(eval_id)
    result = {
        "eval_id": eval_id,
        report.EVALUATION_REQUEST_TO_BUILDING_STARTED_MS_KEY: evaluation_request_to_first_non_evaluating_status_ms(eval_id),
        report.WORKERS_EVALUATION_PHASE_SPAN_MS_KEY: report.evaluation_phase_span_milliseconds(phases),
        "statements": statements,
        "shards": shards,
        "jobs": jobs,
        "phases": phases,
        "shard_timings": shard_timings(eval_id),
        "status": status,
        **captured,
    }
    if status != "Completed":
        span_sizes[server.name] = get_span_trace_file_sizes(server)
        psql("SELECT pg_stat_statements_reset();")
        api("POST", f"evals/{eval_id}", token, body={"method": "abort"})
        try:
            wait_until_settled(eval_id)
        finally:
            result["abort_statements"] = save_slow_statements(f"{label}-abort")
            save_slow_spans(f"{label}-abort", span_sizes[server.name])
    return result


def write_summary(runs, small):
    out = pathlib.Path(server.out_dir)
    (out / "summary.json").write_text(json.dumps({"runs": runs, "small": small}, indent=2))
    text = report.render(runs, small)
    (out / "summary.txt").write_text(text)
    failed = report.verdicts(runs, small)
    (out / "report").mkdir(exist_ok=True)
    (out / "report" / "index.html").write_text(page.render(runs, small, failed, out))
    print(text)
    below = report.speedup_below_target_lines(runs)
    if below:
        print("Speedups below target (report only)\n" + "\n".join(below))


def measure_large(token):
    runs = {}
    for n in WORKER_COUNTS_TO_MEASURE:
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
keep_nixpkgs()

banner("seed the bench repositories")
server.succeed(f"{GIT} config --global --add safe.directory '*'")
server.succeed(f"{GIT} config --global init.defaultBranch main")
server.succeed(f"{GIT} config --global user.email 'nixos@localhost' && {GIT} config --global user.name 'NixOS test'")
seed("large")
seed("small")
psql("CREATE EXTENSION IF NOT EXISTS pg_stat_statements;")
token = login()

try:
    banner("record the costs of the large flake")
    if has_warm_up_eval_cost_rows_for_task("large"):
        print("Cost estimates for the large flake already exist from an earlier warm-up. Skip the warm-up.")
    else:
        use_workers(token, 1)
        measure(token, "large")

    runs = measure_large(token)
    small = measure_small(token, runs) if MEASURE_SMALL_FLAKE else {}
finally:
    sync_vm_disks()
out = pathlib.Path(server.out_dir)
publish(out, out / "report" / "index.html", name="evalbench-multinode")
failed = report.verdicts(runs, small)
assert not failed, "Failed checks\n" + "\n".join(failed)
