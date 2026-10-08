# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

import json
import shlex


def api(machine, base, method, path, token=None, body=None):
    cmd = f"curl -sS -X {method} {shlex.quote(f'{base}/api/v1/{path}')}"
    if token:
        cmd += f" -H 'Authorization: Bearer {token}'"
    if body is not None:
        cmd += f" -H 'Content-Type: application/json' -d {shlex.quote(json.dumps(body))}"
    reply = json.loads(machine.succeed(cmd))
    assert reply.get("error") is False, f"{method} {path}: {reply.get('message')}"
    return reply.get("message")


def assert_serving(machine, base):
    machine.wait_until_succeeds(f"curl -sf {base}/api/v1/health", timeout=600)
    machine.succeed(f"curl -sf {base}/ | grep -qi '<html'")


def login(machine, base, password):
    token = api(machine, base, "POST", "auth/basic/login",
                body={"loginname": "admin", "password": password})
    assert token, "admin login returned an empty token"
    return token


def create_personal_project(machine, base, token, repo):
    api(machine, base, "PUT", "projects", token,
        {"name": "personal", "display_name": "Personal", "description": "standalone"})
    api(machine, base, "PUT", "caches", token,
        {"name": "main", "display_name": "Main", "description": "standalone", "priority": 10})
    for upstream in api(machine, base, "GET", "caches/main/upstream-caches", token):
        api(machine, base, "DELETE", f"caches/main/upstream-caches/{upstream['id']}", token)
    api(machine, base, "POST", "projects/personal/subscribe/main", token)
    api(machine, base, "PUT", "tasks/personal", token,
        {"name": "packages", "display_name": "Packages", "description": "standalone",
         "repository": repo, "wildcard": "packages.*.*"})
    return api(machine, base, "POST", "tasks/personal/packages/evaluate", token)


def wait_for_green_evaluation(machine, base, token, eval_id, timeout=900):
    def completed(_):
        status = api(machine, base, "GET", f"evals/{eval_id}", token)["status"]
        assert status != "Failed", f"evaluation {eval_id} failed"
        return status == "Completed"

    with machine.nested(f"waiting for evaluation {eval_id}"):
        retry(completed, timeout_seconds=timeout)


def assert_imported_derivation_built(machine, base, token, eval_id):
    page = api(machine, base, "GET",
               f"tasks/personal/packages/entry-points?evaluation_id={eval_id}&limit=100", token)
    by_attr = {ep["eval"]: ep for ep in page["entry_points"]}
    imports = [ep for attr, ep in by_attr.items() if attr.startswith("other.") and attr.endswith(".ifd")]
    assert len(imports) == 1, f"expected exactly 1 imported derivation entry point: {sorted(by_attr)}"
    assert imports[0]["ifd"] and imports[0]["build_status"] == "Completed", imports[0]
    imported = next(ep for attr, ep in by_attr.items() if attr.endswith(".imported"))
    assert not imported["ifd"] and imported["build_status"] == "Completed", imported
