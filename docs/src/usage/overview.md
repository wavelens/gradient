# Overview

## Getting Started

1. **Register / Log in** — `/` redirects to login automatically.
2. **Create an organization** — organizations own servers, projects, and caches.
3. **Add a build server** — open the organization, go to **Servers**, and add the server. Then add the organization's SSH public key to the server's `authorized_keys` (see [SSH Keys](#ssh-keys) below).
4. **Create a project** — point it at a Git repository and set an evaluation wildcard.

## Evaluation Wildcard

The wildcard is a dot-separated Nix attribute path selecting which flake outputs to build. Multiple patterns are separated by commas (no spaces). The root level is restricted to: `checks`, `packages`, `formatter`, `legacyPackages`, `nixosConfigurations`, `devShells`, `hydraJobs`.

Two wildcard tokens expand attribute names at a given level:

| Token | Behaviour |
|---|---|
| `*` | **Recursive** — matches all attribute names at this level and, at the trailing position, descends one additional level. Consecutive `*` segments collapse: `packages.*.*` and `packages.*` are equivalent. |
| `#` | **Non-recursive** — matches all attribute names at this level but collects only those where `type == "derivation"`. Does not descend further. Use this to target a specific depth precisely. |

### `*` vs `#`

```
packages.x86_64-linux.*   # finds packages.*.*.*  — recurses past x86_64-linux into each package
packages.x86_64-linux.#   # finds packages that are derivations directly under x86_64-linux, no deeper
```

In practice `*` is the right choice for almost all flakes. Use `#` when a flake nests attrsets inside a package attribute and you do not want those nested attrs collected.

### Exclusions

Prefix a pattern with `!` to remove matching paths from the set built by the preceding include patterns.

```
packages.*,!packages.x86_64-linux.broken
```

Exclusion patterns must be exact paths — they cannot contain `*` or `#`.

### Examples

| Wildcard | What gets built |
|---|---|
| `packages.x86_64-linux.#` | Every derivation directly under `packages.x86_64-linux` |
| `packages.x86_64-linux.*` | Same, but also recurses one level deeper into nested attrsets |
| `packages.#.#` | Every derivation at exactly depth 2 under `packages` (system → package) across all systems — first `#` expands systems, second `#` expands packages non-recursively |
| `checks.x86_64-linux.*` | All checks for x86\_64-linux |
| `packages.*` | Packages for every system (equivalent to `packages.*.*`) |
| `packages.*,checks.*` | Packages and checks for every system |
| `packages.x86_64-linux.*,checks.x86_64-linux.*` | Packages and checks for x86\_64-linux only |
| `nixosConfigurations.#` | All NixOS system configurations |
| `devShells.*` | All dev shells for every system |
| `*` | Everything in all top-level output categories |
| `packages.*,!packages.x86_64-linux.broken` | All packages except one excluded path |

## Evaluations

Click **Start Evaluation** on the project page. Gradient clones the repo, evaluates each wildcard match, and dispatches the resulting derivations to the configured build servers.

The evaluation log page shows per-build status, combined ANSI build output, and an **Abort** button.

Evaluations can also be triggered automatically:

- **GitHub App** — when the App is installed, push events from GitHub trigger evaluations instantly (no polling). See [GitHub App](../configuration.md#github-app).
- **Forge webhooks** — for Gitea, Forgejo, GitLab, or GitHub without the App, configure a per-org push webhook. See [Forge Webhooks](../configuration.md#forge-webhooks-gitea--forgejo--gitlab--github-without-app).
- **Polling** — fallback for projects without webhook configuration; Gradient checks for new commits every 60 seconds.

## SSH Keys

Each organization has one Ed25519 SSH key pair, generated automatically. The public key is shown in **Organization → Settings → SSH**.

Add this key to:

- **Build servers** — in `authorized_keys` so Gradient can connect and run builds.
- **Git hosts** — as a deploy key if your repository is cloned over SSH.

The key is scoped to the organization; different organizations use different keys.
