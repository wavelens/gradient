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
gradient run .#hello
```

| Attribute | Program |
|---|---|
| Exec app with `passthru.gradient` | As declared, see [Declare an Exec App](apps.md) |
| `devShells.*` | Interactive `bash` inside the shell environment, like `nix develop` |
| Other derivations | `bin/<name>` of the package, with `meta.mainProgram`, `pname` or the package name as `<name>` |

- Commands without an attribute run `packages.<system>.default`.
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
- Jobs in interactive mode get `ssh = true`. Only workers with terminals can take the job.

## Verify Deployment

- The output of `gradient run .#hello` is `Hello, world!` together with a zero exit code.
- Refused jobs show the message `Exec jobs don't run in build requests for this project.` with the name of the setting.

## Next Steps

- [Declare an Exec App](apps.md): fixed hardware requests, secrets and images
- [Open a Terminal](terminals.md): browser terminals and SSH logins
- [Turn On Exec Jobs](enable.md): every switch between a repository and a worker
- [CLI](../reference/cli.md): all flags of `gradient run`
