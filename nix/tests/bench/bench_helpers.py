# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

FAILED = {"Failed", "Aborted"}


def banner(msg):
    print(f"\n=== {msg} ===")


def psql_command(query, database="gradient"):
    server.succeed(f"cat > /tmp/q.sql <<'EOF'\n{query}\nEOF")
    return f"su postgres -c 'psql -v ON_ERROR_STOP=1 -d {database} -At -f /tmp/q.sql'"


def psql(query, database="gradient"):
    return server.succeed(psql_command(query, database)).strip()


def dump_json(query, path, database="gradient"):
    server.succeed(
        psql_command(f"SELECT coalesce(json_agg(t), '[]') FROM ({query}) t;", database) + f" > {path}"
    )


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


def wait_for_status(token, eval_id, wanted, timeout):
    # Polled inside the VM: a driver round trip per poll caps the timing at a second.
    stop = "|".join(sorted(wanted | FAILED))
    rc, status = server.execute(
        f"timeout {timeout} sh -c 'while :; do"
        f" s=$(curl -sf {API}/evals/{eval_id} -H \"Authorization: Bearer {token}\" | jq -r .message.status);"
        f" echo \"$s\" | grep -qxE \"{stop}\" && {{ echo \"$s\"; exit 0; }}; sleep 0.2; done'"
    )
    status = status.strip()
    if rc != 0:
        raise Exception(f"evaluation {eval_id} did not reach {wanted} within {timeout} s")
    if status in FAILED:
        raise Exception(f"evaluation {eval_id} {status}:\n"
                        + server.succeed("journalctl -u gradient-server --no-pager -n 100"))
    return status
