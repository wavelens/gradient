# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only
# Shared by the mock-daemon suites; mk.nix prepends FLAKES, RESOLVED and the topology prelude (WORKER_NODES, fleet_units).

API = "http://gradient.local/api/v1"
TOKEN = ""


def banner(msg):
    print(f"\n=== {msg} ===")


def sql(query):
    server.succeed(f"cat > /tmp/q.sql <<'EOF'\n{query}\nEOF")
    return server.succeed("su postgres -c 'psql -v ON_ERROR_STOP=1 -d gradient -At -f /tmp/q.sql'").strip()


def daemon(node, cmd, args=None):
    # A reply of a few hundred KB decodes corrupted when read back over the shell, so it crosses the shared dir.
    name = f"ctl-{node.name}.json"
    node.succeed(f"gradient-daemon ctl {cmd} '{json.dumps(args or {})}' > /tmp/shared/{name}")
    return json.loads((node.shared_dir / name).read_text())


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
    for node, unit in fleet_units():
        print(node.succeed(f"journalctl -u {unit} --no-pager -n 80"))
    for w in WORKER_NODES:
        print(w.succeed("journalctl -u gradient-daemon --no-pager -n 80"))
    raise Exception(f"evaluation {eval_id} is {status}, wanted {want}")


def drv_of(spec, node):
    return RESOLVED[spec]["derivations"][node]["drvPath"]


def out_of(spec, node):
    return RESOLVED[spec]["derivations"][node]["outputs"]["out"]["path"]


def builds_of(spec, node):
    drv = drv_of(spec, node)
    return [(w, e) for w in WORKER_NODES for e in daemon(w, "builds") if drv in e["paths"]]


def only_build(spec, node):
    builds = builds_of(spec, node)
    assert len(builds) == 1, f"{spec}/{node} built {len(builds)} times"
    return builds[0][1]


def uploaded(path):
    store_hash = path.split("/")[-1].split("-")[0]
    return sql(f"SELECT count(*) FROM cached_path WHERE hash = '{store_hash}' AND file_hash IS NOT NULL") != "0"


def assert_clean():
    for w in WORKER_NODES:
        violations = daemon(w, "violations")
        assert violations == [], f"{w.name}: {violations}"


def phase(spec):
    banner(spec)
    publish(spec)
    return evaluate(spec)
