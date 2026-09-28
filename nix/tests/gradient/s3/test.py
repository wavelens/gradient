# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

# A server on a MinIO bucket and a co-located worker: every NAR the evaluation
# and the build push is admitted, uploaded on a presigned URL and committed
# confirmed, and the server stages nothing on its own disk.

import json

API = "http://127.0.0.1:3000/api/v1"
BASE = "/var/lib/gradient"


def banner(msg):
    print(f"\n=== {msg} ===")


def api(method, path, token=None, body=None):
    cmd = f"curl -sS -X {method} {API}/{path}"
    if token:
        cmd += f" -H 'Authorization: Bearer {token}'"
    if body is not None:
        cmd += f" -H 'Content-Type: application/json' -d '{json.dumps(body)}'"
    j = json.loads(machine.succeed(cmd))
    assert j.get("error") is False, f"{method} {path}: {j.get('message')}"
    return j.get("message")


def sql(query):
    return machine.succeed(f"su postgres -c \"psql -d gradient -At -c \\\"{query}\\\"\"").strip()


def metrics():
    return machine.succeed(
        "curl -sS -H 'Authorization: Bearer metricstoken' http://127.0.0.1:3000/metrics"
    )


def metric(name):
    for line in metrics().splitlines():
        if line.startswith(name + " "):
            return float(line.split()[1])
    raise AssertionError(f"metric {name} missing")


start_all()

banner("Bucket and server are up")
machine.wait_for_unit("minio-bucket.service")
machine.wait_for_unit("gradient-server.service")
machine.wait_for_open_port(3000)
machine.succeed("mc alias set local http://127.0.0.1:9000 gradient gradientsecret")

banner("Fixture repository")
sh = machine.succeed("readlink -f $(command -v busybox)").strip()
machine.succeed("git config --global init.defaultBranch main")
machine.succeed("git config --global user.email t@t && git config --global user.name t")
machine.succeed("git init /var/lib/git/test")
machine.succeed(f"sed 's#@sh@#{sh}#' /var/lib/git/flake.nix > /var/lib/git/test/flake.nix")
machine.succeed("git -C /var/lib/git/test add flake.nix && git -C /var/lib/git/test commit -qm fixture")
machine.succeed("chown -R git:git /var/lib/git/test")

banner("Project, cache and task")
admin = api("POST", "auth/basic/login", body={"loginname": "admin", "password": "admin_password"})
api("PUT", "projects", token=admin, body={"name": "demo", "display_name": "Demo", "description": "s3"})
api("PUT", "caches", token=admin, body={
    "name": "democache", "display_name": "Demo Cache", "description": "d", "priority": 10})
api("POST", "projects/demo/subscribe/democache", token=admin)
api("PUT", "tasks/demo", token=admin, body={
    "name": "s3", "display_name": "S3", "description": "s3",
    "repository": "git://127.0.0.1/test", "wildcard": "packages.x86_64-linux.*"})
machine.systemctl("restart gradient-worker.service")
machine.wait_until_succeeds("journalctl -u gradient-worker.service | grep -qi 'authenticated\\|connected'", timeout=120)

banner("Evaluate and build")
api("POST", "tasks/demo/s3/evaluate", token=admin, body={})
machine.wait_until_succeeds(
    "su postgres -c \"psql -d gradient -At -c \\\"select count(*) from cached_path where package = 's3-hello'\\\"\" | grep -qx 1",
    timeout=600,
)

banner("Every committed NAR is confirmed and in the bucket")
rows = int(sql("select count(*) from cached_path where file_hash is not null"))
assert rows > 1, f"the eval's .drv closure and the output should be cached, got {rows} rows"
unconfirmed = int(sql("select count(*) from cached_path where file_hash is not null and not confirmed"))
assert unconfirmed == 0, f"{unconfirmed} rows were committed unconfirmed"
for h in sql("select hash from cached_path where file_hash is not null").splitlines():
    machine.succeed(f"mc stat local/gradient/nars/{h[:2]}/{h[2:]}.nar.zst")

banner("The server staged nothing")
machine.succeed(f"test ! -e {BASE}/nar-staged")
leftover = machine.succeed(f"find {BASE}/nar-partial -name '*.partial' 2>/dev/null | wc -l").strip()
assert leftover == "0", f"{leftover} relay partials on an S3 server"

banner("Upload admission reported its work")
assert metric("gradient_upload_granted_total") > 0, metrics()
machine.wait_until_succeeds(
    "curl -sS -H 'Authorization: Bearer metricstoken' http://127.0.0.1:3000/metrics"
    " | grep -qx 'gradient_upload_in_flight 0'",
    timeout=60,
)
