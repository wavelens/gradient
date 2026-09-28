# Build Before You Push

Uncommitted changes built on the CI workers with `gradient build`, with a `result` link at the end like `nix build`. The laptop needs no builders and no Nix evaluation.

**Requirements:**

- The CLI, logged in, see [CLI](../reference/cli.md#configuration)
- A project with a cache subscription and a worker, see [First project](../get-started/first-project.md)

## 1. Build

Inside the Git working tree:

```sh
gradient build .#hello
```

- The CLI uploads the tracked files, including uncommitted changes; unchanged files are never sent twice.
- The server evaluates the upload under the project's `build-request` task and the workers build the result.
- Logs stream until every build finishes.

| Target | Evaluates |
|---|---|
| none | The wildcard of the `build-request` task |
| `.#hello` | `packages.<system>.hello`; `--system` picks another system |
| `checks.x86_64-linux.#` | Any [wildcard](../reference/wildcards.md) |

## 2. Use the Result

The primary output lands in a `result` symlink, fetched from the project cache into the local store; `--no-link` skips the link. Without Nix on the machine, the CLI downloads the build products into a `result/` folder instead.

## Override Inputs

```sh
gradient build .#hello --override-input nixpkgs github:NixOS/nixpkgs/nixos-unstable
```

Applies to this run only and repeats for several inputs. The evaluation runs on the server, so the reference must be remote (`github:`, `git+ssh://`, `https://`, ...), never a local path. For an override on every run, see [Update Flake Inputs](flake-updates.md).

## Background Runs

```sh
eval=$(gradient build -b)
gradient watch "$eval"
```

| Command | Shows |
|---|---|
| `gradient build -b` | Prints the evaluation ID and returns at once |
| `gradient watch <evaluation>` | A live dashboard: status, builds and a merged log; `f` follows, `q` quits |
| `gradient logs <evaluation>` | The full log of every build, live or finished |

## Verify Deployment

The run ends with every build completed and `result` pointing into `/nix/store`. The UI lists the run under the project's **Build Requests** task.

## Limits

- Only files tracked by Git are uploaded; untracked files are skipped.
- The upload is capped by `http.maxSourceUploadSize`, 512 MiB by default.
- Private `git+ssh://` inputs are fetched with the project's SSH key.

## Next Steps

- [CLI](../reference/cli.md): every command and option
- [Connect an AI Assistant](mcp.md): read the failed build's log from an assistant
