# Build Before Pushing

Uncommitted changes built on the CI workers with `gradient build`, with a `result` link at the end like `nix build`. No builders and no Nix evaluation on the laptop.

**Requirements:**

- The CLI, logged in, see [CLI](../reference/cli.md#configuration)
- A project with a cache subscription and a worker, see [First Project](../get-started/first-project.md)

## 1. Build

`gradient build` must start inside the Git working tree.

```sh
gradient build .#hello
```

- The CLI is uploading the tracked files, including uncommitted changes.
- Unchanged files are never sent twice.
- A worker is evaluating the upload under the project's `build-request` task.
- The workers are building the result.
- Logs are streaming until every build is finished.

| Target | Evaluated |
|---|---|
| none | The wildcard of the `build-request` task |
| `.#hello` | `packages.<system>.hello`, with `--system` picking another system |
| `checks.x86_64-linux.#` | Any [wildcard](../reference/wildcards.md) |

## 2. Use the Result

The primary output is landing in a `result` symlink, fetched from the project cache into the local store. `--no-link` is skipping the link.

The [static binary](../reference/cli.md#install) is lacking Nix support. This binary is downloading the build products into a `result/` folder instead.

## Override Inputs

```sh
gradient build .#hello --override-input nixpkgs github:NixOS/nixpkgs/nixos-unstable
```

The override is applying to this run only. The flag is repeatable for several inputs.

The evaluation is running on a worker. The reference must be remote (`github:`, `git+ssh://`, `https://`, ...), never a local path.

[Update Flake Inputs](flake-updates.md) is covering an override on every run.

## Background Evaluations

```sh
eval=$(gradient build -b)
gradient watch "$eval"
```

| Command | Effect |
|---|---|
| `gradient build -b` | Printing the evaluation ID and returning at once |
| `gradient watch <evaluation>` | A live dashboard with status, builds and a merged log. `f` to follow, `q` to quit |
| `gradient logs <evaluation>` | The full log of every build, live or finished |

## Verify Deployment

The command is ending with every build completed and `result` pointing into `/nix/store`. The UI is listing the run under the project's **Build Requests** task.

## Limits

- The upload is holding only files tracked by Git. Untracked files are left out.
- `http.maxSourceUploadSize` is capping the upload, 512 MiB by default.
- Private `git+ssh://` inputs are using the project's SSH key.

## Next Steps

- [CLI](../reference/cli.md): every command and option
- [Connect an AI Assistant](mcp.md): read the failed build's log from an assistant
