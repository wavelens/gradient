# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

BASE = "http://localhost"

start_all()
machine.wait_for_unit("git-daemon.service")
machine.succeed(
    "git init -b main /srv/git/personal && cp /srv/flake.nix /srv/git/personal/ "
    "&& git -C /srv/git/personal add flake.nix "
    "&& git -C /srv/git/personal -c user.name=t -c user.email=t@t commit -m init "
    "&& chown -R git:git /srv/git/personal"
)

machine.wait_for_unit("gradient-standalone-login.service")
assert_serving(machine, BASE)

password = machine.succeed("cat /var/lib/gradient-standalone/admin-password").strip()
machine.succeed(f"journalctl -u gradient-standalone-login.service | grep -F 'admin / {password}'")
token = login(machine, BASE, password)

eval_id = create_personal_project(machine, BASE, token, "git://localhost/personal")
wait_for_green_evaluation(machine, BASE, token, eval_id)

machine.succeed("systemctl restart gradient-standalone-secrets.service")
assert machine.succeed("cat /var/lib/gradient-standalone/admin-password").strip() == password, \
    "secrets must not be regenerated once they exist"
