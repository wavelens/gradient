# Open a Terminal

Browser terminals, SSH logins and port forwards into a running exec job.

**Requirements:**

- An exec worker with `exec.allow.ssh = true`, see [Set Up an Exec Worker](worker.md)
- The SSH server and a key for SSH logins, see [Build over SSH](../guides/build-over-ssh.md)

## 1. Allow Terminals { #ssh-and-terminal }

Jobs with `ssh = true` accept terminal sessions from the browser and over SSH.

```nix
# flake.nix
apps.x86_64-linux.notebook = {
  type = "app";
  program = "${pkgs.bashInteractive}/bin/bash";
  passthru.gradient = { ssh = true; ports = [ 8888 ]; };
};
```

- Programs of such jobs start on a terminal. `bash` programs wait for input and hold the job up until the shell exits.
- A job with `ssh = false` provides the guest serial console as its only access.
- Sessions need the `TriggerEvaluation` permission on the project, see [Members and Roles](../ui/members-and-roles.md).
- New shells start as `gradient` in `/work` with the environment of the program.
- Interactive programs still obey `maxSilent`. Typed input echoed by the terminal counts as output.

## 2. Use the Terminal Tab

The **Terminal** tab of the job page offers three sessions.

| Session | Job status | Content |
|---|---|---|
| **Console** | `starting` or `running` | Serial console of the guest, logged in as `gradient`, starting with the last 64 KiB of output |
| **Attach to program** | `running`, `ssh = true`, program not exited | The terminal of the program |
| **New shell** | `running`, `ssh = true` | A new shell next to the program |

- The oldest viewer can type into the console. Later viewers see a read-only view.
- Programs stay active past the close of an attached view.
- Multi-node jobs add a **Node** picker. The log and every session follow the picked node.
- Terminals and the console log in as `gradient`. The default guest image has no root login.

## 3. Log In over SSH

| Login | Target |
|---|---|
| `<project>/<job-id>` | The job |
| `<project>/<job-id>/<node>` | One node of a [multi-node job](multi-node.md), `0` for the master |
| `<project>/<task>` | Node 0 of the newest running exec job of the task |

```sh
ssh -p 2222 myproject/<job-id>@ci.example.com # (1)!
ssh -t -p 2222 myproject/<job-id>@ci.example.com shell # (2)!
ssh -p 2222 -N -L 8888:127.0.0.1:8888 myproject/<job-id>@ci.example.com # (3)!
```

1.  Attach to the terminal of the program.
2.  Open a new shell next to the program.
3.  Forward the local port 8888 to port 8888 inside the guest.

- Job pages show the `ssh` command and an `ssh -N -L` command per port once the job is `running`, ready to copy.
- Forwards reach only ports listed in `ports`.
- Logins accept no command except `shell`.
- Logins and forwards into a job before its start wait up to 30 minutes for the guest.
- Waiting logins print `waiting for vm startup`. Sessions close on a job end during the wait.
- Sessions close on a worker disconnect. Programs keep going.

## 4. Keep the VM Running { #keep-vm-running }

Click **Keep VM running** on the job page to hold the guest up after the program exit. The switch is available before the start and during the job.

| Event | Result |
|---|---|
| Program exit | The job stays `running`, with the notice "Program exited with code N. The VM keeps running." |
| **Let VM stop**, or a second click on **Keep VM running** | End as `completed` or `failed` by the recorded exit code |
| **Abort** | End as `aborted` |
| Guest shutdown | End by the recorded exit code |

- New shells, SSH logins and the console remain usable on a held guest.
- Program exits end `maxSilent`. The job `timeout` is in force.
- All nodes of a multi-node job follow the switch.

## 5. Shut Down the Guest

`poweroff`, `shutdown`, `reboot`, `halt` and `systemctl poweroff` run in every terminal and on the console. The user `gradient` can run them without a password.

- Shutdowns during the program leave the job `aborted`, same as **Abort**. The job won't restart.
- Shutdowns of a [held guest](#keep-vm-running) close the job by the recorded exit code.

## Verify Deployment

- The **Terminal** tab of a running job opens the **Console**.
- `ssh -p 2222 <project>/<job-id>@<host>` attaches to the program.

## Next Steps

- [Build over SSH](../guides/build-over-ssh.md): SSH server and key setup
- [Members and Roles](../ui/members-and-roles.md): the `TriggerEvaluation` permission
- [Spread a Job over Workers](multi-node.md): node logins
