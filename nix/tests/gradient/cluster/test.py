# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only
# mk.nix prepends FLAKES, RESOLVED, WORKER_IDS, the topology prelude and ../scheduler/helpers.py.
# No producer creates cluster jobs yet: members are anchors held behind a hanging `gate`, seeded by SQL meanwhile.

import uuid

NODES = {node.name: node for node in WORKER_NODES}


def anchor_of(spec, node):
    drv_hash = drv_of(spec, node).split("/")[-1].split("-")[0]
    return sql(
        "SELECT db.id FROM derivation_build db JOIN derivation d ON d.id = db.derivation "
        f"WHERE d.hash = '{drv_hash}'"
    )


def wait_running(spec, node, timeout=300):
    for _ in range(timeout):
        running = [w for w in WORKER_NODES if f"{spec}/{node}" in daemon(w, "running")]
        if running:
            return running[0]
        server.sleep(1)
    raise Exception(f"{spec}/{node} never started building")


def hold_members(spec, members):
    gate = wait_running(spec, "gate")
    for _ in range(120):
        if all(anchor_of(spec, m) for m in members):
            break
        server.sleep(1)
    for m in members:
        status = sql(f"SELECT status FROM derivation_build WHERE id = '{anchor_of(spec, m)}'")
        assert status == "0", f"{spec}/{m} is {status} before its cluster exists"
    return gate


def seed_cluster(spec, members, same_zone, retry_budget):
    cluster = str(uuid.uuid4())
    rows = [
        "INSERT INTO cluster_job (id, status, same_zone, attempts, retry_budget, created_at, updated_at) "
        f"VALUES ('{cluster}', 0, {str(same_zone).lower()}, 0, {retry_budget}, now(), now());"
    ]
    for node, role, pin in members:
        pin_sql = f"'{pin}'" if pin else "NULL"
        rows.append(
            'INSERT INTO cluster_member (id, cluster_job, derivation_build, role, "primary", pin) '
            f"VALUES (gen_random_uuid(), '{cluster}', '{anchor_of(spec, node)}', '{role}', false, {pin_sql});"
        )
    sql("\n".join(rows))
    return cluster


def cluster_status(cluster):
    return sql(f"SELECT status FROM cluster_job WHERE id = '{cluster}'")


def attempts_of(cluster):
    rows = sql(
        "SELECT id, coalesce(outcome::text, ''), (started_at IS NOT NULL)::text FROM cluster_attempt "
        f"WHERE cluster_job = '{cluster}' ORDER BY created_at"
    )
    return [row.split("|") for row in rows.splitlines() if row]


def seats_of(attempt):
    rows = sql(f"SELECT job_id, worker_id FROM dispatched_job WHERE cluster_attempt = '{attempt}'")
    return dict(row.split("|") for row in rows.splitlines() if row)


def member_key(spec, node):
    return f"build:{anchor_of(spec, node)}"


def assert_no_single_member_rows(spec, members):
    keys = ", ".join(f"'{member_key(spec, m)}'" for m in members)
    singles = sql(f"SELECT count(*) FROM dispatched_job WHERE job_id IN ({keys}) AND cluster_attempt IS NULL")
    assert singles == "0", f"{singles} member rows were dispatched outside their cluster"


start_all()
server.wait_for_unit("gradient-server.service")
for w in WORKER_NODES:
    w.wait_for_unit("gradient-daemon.service")
wait_workers_ready()
login()

banner("same zone, pinned member")
e = phase("zone-pair")
gate = hold_members("zone-pair", ["m1", "m2"])
cluster = seed_cluster(
    "zone-pair",
    [("m1", "left", WORKER_IDS["worker2"]), ("m2", "right", None)],
    same_zone=True,
    retry_budget=0,
)
daemon(gate, "release", {"node": "zone-pair/gate"})
wait_evaluation(e, "Completed")
assert cluster_status(cluster) == "2", cluster_status(cluster)
[[attempt, outcome, started]] = attempts_of(cluster)
assert (outcome, started) == ("0", "true"), (outcome, started)
seats = seats_of(attempt)
assert seats == {
    member_key("zone-pair", "m1"): WORKER_IDS["worker2"],
    member_key("zone-pair", "m2"): WORKER_IDS["worker1"],
}, seats
assert_no_single_member_rows("zone-pair", ["m1", "m2"])
for name in ["worker1", "worker2"]:
    NODES[name].succeed(
        f"journalctl -u gradient-worker --no-pager | grep 'cluster started' | grep {attempt}"
        f" | grep {WORKER_IDS['worker1']} | grep -q {WORKER_IDS['worker2']}"
    )
NODES["worker3"].fail(f"journalctl -u gradient-worker --no-pager | grep 'cluster started' | grep -q {attempt}")
assert_clean()

banner("member lost mid attempt")
e = phase("kill-pair")
gate = hold_members("kill-pair", ["k1", "k2"])
cluster = seed_cluster("kill-pair", [("k1", "left", None), ("k2", "right", None)], same_zone=False, retry_budget=2)
daemon(gate, "release", {"node": "kill-pair/gate"})
victim = wait_running("kill-pair", "k1")
holder = wait_running("kill-pair", "k2")
assert victim is not holder, "both members ran on one worker"
# A build reads its outcome when it starts: the hanging first attempt stays hung, every retry succeeds.
for w in WORKER_NODES:
    if w is not victim:
        for n in ["k1", "k2"]:
            daemon(w, "outcome", {"node": f"kill-pair/{n}", "outcome": "success"})
[[first, _, _]] = attempts_of(cluster)
victim.succeed("systemctl kill --signal=KILL gradient-worker && systemctl stop gradient-worker")
for _ in range(180):
    if sql(f"SELECT finished_at IS NOT NULL FROM cluster_attempt WHERE id = '{first}'") == "t":
        break
    server.sleep(1)
else:
    raise Exception(f"attempt {first} stayed open after its member was lost")
assert sql(f"SELECT count(*) FROM dispatched_job WHERE cluster_attempt = '{first}' AND finished_at IS NULL") == "0"
wait_evaluation(e, "Completed", timeout=600)
assert cluster_status(cluster) == "2", cluster_status(cluster)
assert sql(f"SELECT attempts FROM cluster_job WHERE id = '{cluster}'") == "2"
attempts = attempts_of(cluster)
assert len(attempts) == 2, attempts
assert attempts[0][0] == first and attempts[0][1] in ("1", "3"), attempts[0]
assert attempts[1][1:] == ["0", "true"], attempts[1]
assert sql(f"SELECT count(*) FROM cluster_attempt WHERE cluster_job = '{cluster}' AND finished_at IS NULL") == "0"
seats = seats_of(attempts[1][0])
assert len(set(seats.values())) == 2, seats
assert WORKER_IDS[victim.name] not in seats.values(), seats
assert_no_single_member_rows("kill-pair", ["k1", "k2"])
daemon(victim, "release", {"node": "kill-pair/k1"})
victim.succeed("systemctl start gradient-worker")
victim.wait_until_succeeds(
    "journalctl -u gradient-worker --no-pager --since=-180s | grep -q 'handshake successful'", timeout=180
)
assert_clean()
