# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

from datetime import timedelta

BASE = "http://localhost:8080"
RUN = ("docker run -dt --name gradient --privileged --cgroupns=host "
       "-p 127.0.0.1:8080:80 -v gradient:/var/lib gradient-standalone:latest")
LOGIN_JOURNAL = "docker exec gradient journalctl -u gradient-standalone-login.service --no-pager"


def dump_container():
    for cmd in [
        "docker ps -a",
        "docker logs gradient 2>&1 | tail -n 200",
        "docker exec gradient systemctl is-system-running",
        "docker exec gradient systemctl --failed --no-pager",
        "docker exec gradient journalctl -b --no-pager -n 300",
    ]:
        print(f"=== {cmd} ===\n{machine.execute(cmd)[1]}")


def start_container():
    machine.succeed(RUN)
    try:
        machine.wait_until_succeeds(f"{LOGIN_JOURNAL} | grep -q 'log in with admin / '", timeout=timedelta(seconds=600))
        assert_serving(machine, BASE)
    except Exception:
        dump_container()
        raise


start_all()
machine.wait_for_unit("docker.service")
machine.wait_for_unit("git-daemon.service")
machine.succeed(
    "git init -b main /srv/git/personal && cp /srv/flake.nix /srv/git/personal/ "
    "&& git -C /srv/git/personal add flake.nix "
    "&& git -C /srv/git/personal -c user.name=t -c user.email=t@t commit -m init "
    "&& chown -R git:git /srv/git/personal"
)
machine.succeed("docker load < /etc/gradient-standalone.tar.gz")

start_container()
password = machine.succeed("docker exec gradient cat /var/lib/gradient-standalone/admin-password").strip()
machine.succeed(f"{LOGIN_JOURNAL} | grep -F 'admin / {password}'")
token = login(machine, BASE, password)

gateway = machine.succeed(
    "docker network inspect bridge -f '{{(index .IPAM.Config 0).Gateway}}'").strip()
eval_id = create_personal_project(machine, BASE, token, f"git://{gateway}/personal")
wait_for_green_evaluation(machine, BASE, token, eval_id)

machine.succeed("docker stop -t 120 gradient && docker rm gradient")
start_container()
token = login(machine, BASE, password)
status = api(machine, BASE, "GET", f"evals/{eval_id}", token)["status"]
assert status == "Completed", f"evaluation lost across a container recreate: {status}"
