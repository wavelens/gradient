<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# Build Before Pushing

Push work to CI workers and build there with `gradient build`. The workers build uncommitted changes. The output shows a `result` link at the end, the same way `nix build` does. The laptop needs no builders and no Nix evaluation.

**Requirements:**

- The CLI, logged in, see [CLI](../reference/cli.md#configuration)
- A project with a cache subscription and a worker, see [First Project](../get-started/first-project.md)

## 1. Build

Run `gradient build` from inside the Git working tree.

```sh
gradient build .#hello
```

- The CLI uploads all tracked files, including uncommitted changes.
- Files without changes skip a second upload.
- A worker evaluates the upload as the project's `build-request` task.
- Workers produce the build results.
- Logs stream until every build finishes.

| Target | Evaluated |
|---|---|
| none | The wildcard of the `build-request` task |
| `.#hello` | `packages.<system>.hello`, with `--system` selecting a different system |
| `checks.x86_64-linux.*` | Any [wildcard](../reference/wildcards.md) |

## 2. Use the Result

The build output lands in a `result` symlink. The symlink points into the local store, populated from the project cache. Pass `--no-link` to skip the link creation.

Multiple entry points produce `result-1`, `result-2` and more links, matching `nix build` behavior. A failed evaluation or build exits with code `1`, even with `--no-link`.

The local Nix daemon trusts fetched paths only if the cache public key sits in `trusted-public-keys`. Setup steps live in [Share a Cache](share-a-cache.md). Users in `trusted-users` need no key.

[Static binaries](../reference/cli.md#install) come without Nix support. These binaries download build products into `result/`, `result-1/` and further folders instead.

## Override Inputs

```sh
gradient build .#hello --override-input nixpkgs github:NixOS/nixpkgs/nixos-unstable
```

The `--override-input` flag replaces a flake input for a single build. Pass it once per input. Workers perform the evaluation, so the replacement must point to a remote source such as `github:`, `git+ssh://` or `https://`. Local paths do not work.

To apply an override on every build, see [Update Flake Inputs](flake-updates.md).

## Background Evaluations

```sh
eval=$(gradient build -b)
gradient watch "$eval"
```

| Command | Effect |
|---|---|
| `gradient build -b` | Run in the background. The command prints the evaluation ID and exits immediately |
| `gradient watch <evaluation>` | Open a live dashboard with status, builds and a merged log. Press `f` to follow output, `q` to quit |
| `gradient logs <evaluation>` | Print the complete log of all builds, whether live or finished |

## Verify Deployment

A successful run ends with all builds finished and `result` resolved to a path in `/nix/store`. The run also appears in the project's **Build Requests** task list in the UI.

## Limits

- Uploads contain only Git-tracked files. Untracked files stay out.
- `http.maxSourceUploadSize` caps the upload size. The default is 512 MiB.
- Private `git+ssh://` inputs authenticate with the project's SSH key.

## Next Steps

- [CLI](../reference/cli.md): every command and option
- [Connect an AI Assistant](mcp.md): read the failed build's log from an assistant
