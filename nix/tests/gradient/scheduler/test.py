# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only
# FLAKES and RESOLVED are prepended by default.nix: spec name -> flake dir / resolved store-spec.
import json

WORKERS = [worker1, worker2]
API = "http://gradient.local/api/v1"
TOKEN = ""


def banner(msg):
    print(f"\n=== {msg} ===")


def sql(query):
    server.succeed(f"cat > /tmp/q.sql <<'EOF'\n{query}\nEOF")
    return server.succeed("su postgres -c 'psql -d gradient -At -f /tmp/q.sql'").strip()


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


def publish(name):
    work = f"/tmp/spec-{name}"
    server.succeed(
        f"rm -rf {work} /var/lib/git/{name}"
        f" && cp -r {FLAKES[name]} {work} && chmod -R u+w {work}"
        f" && git -C {work} init -q && git -C {work} add -A"
        f" && git -C {work} -c user.email=t@t -c user.name=t commit -qm spec"
        f" && git clone -q --bare {work} /var/lib/git/{name}"
    )


def evaluate(name):
    return api("POST", f"/tasks/project/{name}/evaluate", {})


def wait_evaluation(eval_id, want, timeout=300):
    status = ""
    for _ in range(timeout):
        status = api("GET", f"/evals/{eval_id}")["status"]
        if status == want:
            return
        if status in ("Completed", "Failed", "Aborted"):
            break
        server.sleep(1)
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


def phase(name):
    banner(name)
    publish(name)
    return evaluate(name)


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
chain = [only_build("chain-3", n) for n in ["c0", "c1", "c2"]]
for dep, top in zip(chain, chain[1:]):
    assert top["at_us"] >= dep["at_us"] + dep["duration_us"], "a build started before its input was built"
for n in ["c0", "c1", "c2"]:
    assert uploaded(out_of("chain-3", n)), n
assert_clean()

e = phase("diamond-fail")
wait_evaluation(e, "Failed")
assert builds_of("diamond-fail", "top") == []
only_build("diamond-fail", "right")
assert_clean()

e = phase("cross-worker")
wait_evaluation(e, "Completed")
dep_build = builds_of("cross-worker", "dep")
top_build = builds_of("cross-worker", "top")
assert [w.name for w, _ in dep_build] == ["worker1"], dep_build
assert [w.name for w, _ in top_build] == ["worker2"], top_build
dep_out = out_of("cross-worker", "dep")
journal = daemon(worker2, "journal")
imported = min(e["at_us"] + e["duration_us"] for e in journal if e["op"] == "add_to_store_nar" and dep_out in e["paths"])
assert imported <= top_build[0][1]["at_us"], "worker2 built top before it had dep"
assert_clean()

e = phase("already-present")
wait_evaluation(e, "Completed")
assert all(builds_of("already-present", n) == [] for n in ["c0", "c1"])
assert_clean()

e = phase("upstream-cached")
wait_evaluation(e, "Completed")
assert builds_of("upstream-cached", "lib") == []
only_build("upstream-cached", "app")
assert_clean()
