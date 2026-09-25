# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only
# FLAKES and RESOLVED are prepended by default.nix: spec file -> flake dir / resolved store-spec.
# Build order is checked by each mock daemon (BuildWithMissingInput), never across VM clocks.

WORKERS = [worker1, worker2]
API = "http://gradient.local/api/v1"
TOKEN = ""


def banner(msg):
    print(f"\n=== {msg} ===")


def sql(query):
    server.succeed(f"cat > /tmp/q.sql <<'EOF'\n{query}\nEOF")
    return server.succeed("su postgres -c 'psql -v ON_ERROR_STOP=1 -d gradient -At -f /tmp/q.sql'").strip()


def daemon(node, cmd, args=None):
    return json.loads(node.succeed(f"gradient-daemon ctl {cmd} '{json.dumps(args or {})}'"))


def login():
    global TOKEN
    body = json.dumps({"loginname": "admin", "password": "admin_password"})
    reply = server.wait_until_succeeds(
        f"curl -sf -X POST -H 'Content-Type: application/json' -d '{body}' {API}/auth/basic/login",
        timeout=180,
    )
    TOKEN = json.loads(reply)["message"]


def api(method, path, body=None):
    data = f"-H 'Content-Type: application/json' -d '{json.dumps(body)}'" if body is not None else ""
    reply = server.succeed(f"curl -sf -X {method} -H 'Authorization: Bearer {TOKEN}' {data} {API}{path}")
    return json.loads(reply)["message"]


def repo_of(spec):
    return RESOLVED[spec]["name"]


def publish(spec):
    work = f"/tmp/spec-{spec}"
    repo = f"/var/lib/git/{repo_of(spec)}"
    server.succeed(
        f"rm -rf {work} {repo}"
        f" && cp -r {FLAKES[spec]} {work} && chmod -R u+w {work}"
        f" && git -C {work} init -q && git -C {work} add -A"
        f" && git -C {work} -c user.email=t@t -c user.name=t commit -qm {spec}"
        f" && git clone -q --bare {work} {repo}"
    )


def evaluate(spec):
    return api("POST", f"/tasks/project/{repo_of(spec)}/evaluate", {})


def wait_evaluation(eval_id, want, timeout=300):
    status = ""
    for _ in range(timeout):
        seen, status = status, api("GET", f"/evals/{eval_id}")["status"]
        if status != seen:
            print(f"evaluation {eval_id}: {status}")
        if status == want:
            return
        if status in ("Completed", "Failed", "Aborted"):
            break
        server.sleep(1)
    print(sql(
        "SELECT d.name, db.status FROM build_job bj JOIN derivation_build db ON db.id = bj.derivation_build "
        f"JOIN derivation d ON d.id = db.derivation WHERE bj.evaluation = '{eval_id}' ORDER BY d.name"
    ))
    print(server.succeed("journalctl -u gradient-server --no-pager -n 120"))
    for w in WORKERS:
        print(w.succeed("journalctl -u gradient-daemon -u gradient-worker --no-pager -n 80"))
    raise Exception(f"evaluation {eval_id} is {status}, wanted {want}")


def drv_of(spec, node):
    return RESOLVED[spec]["derivations"][node]["drvPath"]


def out_of(spec, node):
    return RESOLVED[spec]["derivations"][node]["outputs"]["out"]["path"]


def builds_of(spec, node):
    drv = drv_of(spec, node)
    return [(w, e) for w in WORKERS for e in daemon(w, "builds") if drv in e["paths"]]


def only_build(spec, node):
    builds = builds_of(spec, node)
    assert len(builds) == 1, f"{spec}/{node} built {len(builds)} times"
    return builds[0][1]


def uploaded(path):
    store_hash = path.split("/")[-1].split("-")[0]
    return sql(f"SELECT count(*) FROM cached_path WHERE hash = '{store_hash}' AND file_hash IS NOT NULL") != "0"


def assert_clean():
    for w in WORKERS:
        violations = daemon(w, "violations")
        assert violations == [], f"{w.name}: {violations}"


def phase(spec):
    banner(spec)
    publish(spec)
    return evaluate(spec)


def merged_journal():
    rows = []
    for w in WORKERS:
        rows += [dict(e, worker=w.name) for e in daemon(w, "journal")]
    return sorted(rows, key=lambda e: e["at_us"])


def pct(xs, q):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(round((len(xs) - 1) * q)))] if xs else 0


def spread(xs):
    return {"p50": pct(xs, 0.5), "p95": pct(xs, 0.95), "max": max(xs or [0])}


def critical_path_ms(nodes, build_ms):
    memo = {}

    def longest(node):
        if node not in memo:
            memo[node] = build_ms.get(node, 0) + max([longest(d) for d in nodes[node]["deps"]] or [0])
        return memo[node]

    return max([longest(n) for n in nodes] or [0])


def latency_report(spec):
    nodes = RESOLVED[spec]["derivations"]
    journal = merged_journal()
    valid_at: dict[str, int] = {}
    for e in journal:
        if e["ok"] and e["op"] in ("add_to_store_nar", "build_derivation"):
            for p in e["paths"]:
                valid_at.setdefault(p, e["at_us"] + e["duration_us"])
    for name, node in nodes.items():
        b = next((e for e in journal if e["op"] == "build_derivation" and node["drvPath"] in e["paths"]), None)
        if b:
            for out in node["outputs"].values():
                valid_at.setdefault(out["path"], b["at_us"] + b["duration_us"])

    ready_to_dispatch: list[float] = []
    build_ms: dict[str, float] = {}
    first: int | None = None
    last = 0
    for name, node in nodes.items():
        b = next((e for e in journal if e["op"] == "build_derivation" and node["drvPath"] in e["paths"]), None)
        if not b:
            continue
        inputs = [o["path"] for d in node["deps"] for o in nodes[d]["outputs"].values()]
        ready = max([valid_at.get(i, b["at_us"]) for i in inputs] or [b["at_us"]])
        ready_to_dispatch.append((b["at_us"] - ready) / 1000)
        build_ms[name] = b["duration_us"] / 1000
        first = b["at_us"] if first is None else min(first, b["at_us"])
        last = max(last, b["at_us"] + b["duration_us"])

    wall_ms = (last - (first or 0)) / 1000
    critical = critical_path_ms(nodes, build_ms)
    report = {
        "spec": spec,
        "builds": len(build_ms),
        "wall_ms": wall_ms,
        "critical_path_ms": critical,
        "overhead_factor": wall_ms / critical if critical else None,
        "ready_to_dispatch_ms": spread(ready_to_dispatch),
        "build_ms": spread(list(build_ms.values())),
        "ops": {w.name: daemon(w, "latency") for w in WORKERS},
    }
    print(json.dumps(report, indent=2))
    server.succeed(f"mkdir -p /tmp/xchg-out && echo '{json.dumps(report)}' >> /tmp/xchg-out/latency.jsonl")
    for w in WORKERS:
        daemon(w, "reset-journal")
    return report


start_all()
server.wait_for_unit("gradient-server.service")
for w in WORKERS:
    w.wait_for_unit("gradient-daemon.service")
    w.wait_for_unit("gradient-worker.service")
    w.wait_until_succeeds(
        "journalctl -u gradient-worker --no-pager | grep -q 'handshake successful'", timeout=180
    )
login()

e = phase("chain-3")
wait_evaluation(e, "Completed")
for n in ["c0", "c1", "c2"]:
    only_build("chain-3", n)
    assert uploaded(out_of("chain-3", n)), n
assert_clean()
latency_report("chain-3")

e = phase("diamond-fail")
wait_evaluation(e, "Failed")
assert builds_of("diamond-fail", "top") == []
only_build("diamond-fail", "right")
assert_clean()
latency_report("diamond-fail")

e = phase("cross-worker")
wait_evaluation(e, "Completed")
dep_build = builds_of("cross-worker", "dep")
top_build = builds_of("cross-worker", "top")
assert [w.name for w, _ in dep_build] == ["worker1"], dep_build
assert [w.name for w, _ in top_build] == ["worker2"], top_build
dep_out = out_of("cross-worker", "dep")
imported = min(
    e["at_us"] + e["duration_us"]
    for e in daemon(worker2, "journal")
    if e["op"] == "add_to_store_nar" and dep_out in e["paths"]
)
assert imported <= top_build[0][1]["at_us"], "worker2 built top before it had dep"
assert_clean()
latency_report("cross-worker")

e = phase("already-present")
wait_evaluation(e, "Completed")
assert all(builds_of("already-present", n) == [] for n in ["c0", "c1"])
assert_clean()
latency_report("already-present")

e = phase("upstream-cached")
wait_evaluation(e, "Completed")
assert builds_of("upstream-cached", "lib") == []
only_build("upstream-cached", "app")
assert_clean()
latency_report("upstream-cached")

banner("unchanged commit")
wait_evaluation(evaluate("chain-3"), "Completed")
assert all(daemon(w, "builds") == [] for w in WORKERS), "an unchanged commit rebuilt something"
assert_clean()

banner("forgotten output")
for w in WORKERS:
    for n in ["c0", "c1", "c2"]:
        daemon(w, "forget", {"node": f"chain-3/{n}"})
e = phase("chain-4")
wait_evaluation(e, "Completed")
assert all(builds_of("chain-4", n) == [] for n in ["c0", "c1", "c2"]), "a forgotten output was rebuilt instead of fetched"
builder = builds_of("chain-4", "c3")
assert len(builder) == 1, builder
refetched = [
    entry
    for entry in daemon(builder[0][0], "journal")
    if entry["op"] == "add_to_store_nar" and out_of("chain-4", "c0") in entry["paths"]
]
assert refetched, "c3 built without its forgotten input c0 being fetched back"
assert_clean()
latency_report("chain-4")

banner("worker lost mid build")
e = phase("hang")
hanging = None
for _ in range(300):
    hanging = next((w for w in WORKERS if "hang/c1" in daemon(w, "running")), None)
    if hanging:
        break
    server.sleep(1)
assert hanging, "c1 never started building"
survivor = next(w for w in WORKERS if w is not hanging)
daemon(survivor, "outcome", {"node": "hang/c1", "outcome": "success"})
hanging.succeed("systemctl kill --signal=KILL gradient-worker && systemctl stop gradient-worker")
wait_evaluation(e, "Completed")
assert [w.name for w, entry in builds_of("hang", "c1") if entry["ok"]] == [survivor.name]
daemon(hanging, "release", {"node": "hang/c1"})
hanging.succeed("systemctl start gradient-worker")
hanging.wait_until_succeeds(
    "journalctl -u gradient-worker --no-pager --since=-120s | grep -q 'handshake successful'", timeout=180
)
assert_clean()
latency_report("hang")

banner("worker frozen mid build")
e = phase("frozen")
frozen = None
for _ in range(300):
    frozen = next((w for w in WORKERS if "frozen/c1" in daemon(w, "running")), None)
    if frozen:
        break
    server.sleep(1)
assert frozen, "c1 never started building"
survivor = next(w for w in WORKERS if w is not frozen)
daemon(survivor, "outcome", {"node": "frozen/c1", "outcome": "success"})
since = frozen.succeed("date +%s").strip()
server_since = server.succeed("date +%s").strip()
frozen.succeed("systemctl kill --signal=STOP gradient-worker")
server.wait_until_succeeds(
    f"journalctl -u gradient-server --no-pager --since=@{server_since} | grep -q 'presumed dead'", timeout=120
)
wait_evaluation(e, "Completed")
frozen.succeed("systemctl kill --signal=CONT gradient-worker")
frozen.wait_until_succeeds(
    f"journalctl -u gradient-worker --no-pager --since=@{since} | grep -q 'reconnected successfully'", timeout=120
)
assert [w.name for w, entry in builds_of("frozen", "c1") if entry["ok"]] == [survivor.name]
daemon(frozen, "release", {"node": "frozen/c1"})
assert_clean()
latency_report("frozen")

e = phase("stress")
wait_evaluation(e, "Completed", timeout=900)
assert_clean()
report = latency_report("stress")
assert report["builds"] == len(RESOLVED["stress"]["derivations"]), report["builds"]

banner("replay")
nodes = RESOLVED["replay"]["derivations"]
needed, frontier = set(), [n for n in RESOLVED["replay"]["entryPoints"] if not nodes[n]["present"]["cache"]]
while frontier:
    n = frontier.pop()
    if n not in needed:
        needed.add(n)
        frontier += [d for d in nodes[n]["deps"] if not nodes[d]["present"]["cache"]]
failing = {n for n in needed if nodes[n]["build"]["outcome"] == "fail"}
blocked = set(failing)
while True:
    grown = {n for n, node in nodes.items() if set(node["deps"]) & blocked} | blocked
    if grown == blocked:
        break
    blocked = grown
e = phase("replay")
wait_evaluation(e, "Failed" if failing else "Completed", timeout=900)
by_drv = {}
for w in WORKERS:
    for entry in daemon(w, "builds"):
        for p in entry["paths"]:
            by_drv.setdefault(p, []).append((w, entry))
built = {n: by_drv.get(node["drvPath"], []) for n, node in nodes.items() if not node["present"]["cache"]}
for n, builds in built.items():
    oks = [entry["ok"] for _, entry in builds]
    if n in failing:
        assert oks == [False], f"{n}: {builds}"
    elif n in blocked:
        assert builds == [], f"{n} built although a dependency failed"
    elif n in needed and not failing:
        assert oks == [True], f"{n} built {oks}"
    else:
        assert oks in ([], [True]), f"{n} built {oks}"
assert_clean()
latency_report("replay")

server.copy_from_machine("/tmp/xchg-out/latency.jsonl", "")
