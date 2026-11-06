<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# Run a Program from the CLI

A flake program from the working tree, started on an exec worker and streamed back to the local terminal.

**Requirements:**

- An exec worker, see [Set Up an Exec Worker](worker.md)
- The Gradient CLI with a login, see [CLI](../reference/cli.md)

## 1. Allow Exec Jobs in Build Requests

Programs from `gradient run` arrive as build requests on the server. Exec jobs in build requests need the project setting below.

=== "UI"

    Open the project **Settings**, tick **Allow Exec Jobs in Build-Requests** and save.

=== "Declarative"

    ```nix
    services.gradient.state.projects.my-project.allow_exec_in_build_requests = true;
    ```

- The setting covers every exec job in build requests, also exec apps from `gradient build`.
- The instance and worker switches still apply, see [Turn On Exec Jobs](enable.md).

## 2. Start the Program

```sh
gradient run .#packages.x86_64-linux.hello
```

| Attribute | Program |
|---|---|
| Exec app with `passthru.gradient` | As declared, see [Declare an Exec App](apps.md) |
| Other apps with `type = "app"` | The declared `program`, like `nix run` |
| `devShells.*` | Interactive `bash` inside the shell environment, like `nix develop` |
| Other derivations | `bin/<name>` of the package, with `meta.mainProgram`, `pname` or the package name as `<name>` |

- Commands without an attribute run `apps.<system>.default`.
- Bare `.#name` values map to `apps.<system>.name`.
- A job can retry once unless apps declare their own `retries`.
- Build logs of the program stay visible until the job start.

## 3. Pick a Mode

| Mode | Use | Behavior |
|---|---|---|
| Interactive | A single matched job and a local terminal | The local terminal attaches to the program terminal. The program waits for this attach |
| Log | `--no-shell`, more matched jobs, or no terminal | The CLI follows the job logs. Lines get the attribute name in front when more jobs match |

- Exit codes of the CLI equal the exit code of the program. Jobs in log mode end with the first non-zero code.
- Interactive mode forwards every key to the program, `Ctrl-C` included.
- Typing `~.` at the start of a line can abort the job.
- `Ctrl-C` in log mode can abort every unfinished job.

## 4. Reattach

Lost connections end with `Connection closed unexpectedly. Use gradient run --attach <job-id>`. Jobs keep going until their timeout.

```sh
gradient run --attach <job-id>
```

- Reattached terminals continue at the current output. Job logs keep the output from the gap.

## 5. Set Resources

| Flag | Job field |
|---|---|
| `--cpus <N>` | `cpus` |
| `--memory <MB>` | `memoryMb` |
| `--network` | `network = true` |
| `--labels a,b` | `labels` |
| `--timeout <D>` | `timeout` and `maxSilent`, in seconds or like `30m` |

- Flags override the same fields of an exec app. Worker limits still apply.
- Interactive jobs get `ssh = true`. Only workers with terminals can take them. Jobs in log mode keep the `ssh` value of the app or the worker.

## 6. Forward Ports

```sh
gradient run -L 8888 .#notebook # (1)!
gradient run --attach <job-id> -L 9000:8888 # (2)!
```

1. Local port 8888 -> guest port 8888.
2. Local port 9000 -> guest port 8888 on the running job.

- New jobs add each guest port to `ports` and get `ssh = true`.
- Running jobs accept forwards only to ports in their own `ports`.
- Forwards need exactly one matching job.
- Forwards stay open until CLI shutdown. Skip SSH setup.

## Verify Deployment

- The output of `gradient run .#packages.x86_64-linux.hello` is `Hello, world!` together with a zero exit code.
- Refused jobs show the message `Exec jobs don't run in build requests for this project.` with the name of the setting.

## Next Steps

- [Declare an Exec App](apps.md): fixed hardware requests, secrets and images
- [Open a Terminal](terminals.md): browser terminals and SSH logins
- [Turn On Exec Jobs](enable.md): every switch between a repository and a worker
- [CLI](../reference/cli.md): all flags of `gradient run`
