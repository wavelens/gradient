# Overview

## Getting Started

1. **Register / Log in** — `/` redirects to login automatically.
2. **Create an organization** — organizations own projects, caches, and workers.
3. **Create a project** — point it at a Git repository and set an evaluation wildcard.
4. **Configure a worker** — at least one `gradient-worker` must be connected to run jobs. Deploy one co-located on the server or on a dedicated build machine (see [Workers](#workers) below).

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

Click **Start Evaluation** on the project page. Gradient clones the repo, evaluates each wildcard match, and dispatches the resulting derivations to connected workers.

The evaluation log page shows per-build status, combined ANSI build output, and an **Abort** button.

Evaluations can also be triggered automatically:

- **GitHub App** — when the App is installed, push events from GitHub trigger evaluations instantly (no polling). See [GitHub App](../configuration.md#github-app).
- **Forge webhooks** — for Gitea, Forgejo, GitLab, or GitHub without the App, configure a per-org push webhook. See [Forge Webhooks](../configuration.md#forge-webhooks-gitea--forgejo--gitlab--github-without-app).
- **Polling** — fallback for projects without webhook configuration; Gradient checks for new commits every 60 seconds.

## SSH Keys

Each organization has one Ed25519 SSH key pair, generated automatically. The public key is shown in **Organization → Settings → SSH**.

Add this key to your **Git hosts** as a deploy key so Gradient can clone private repositories.

The key is scoped to the organization; different organizations use different keys.

## Workers

Build capacity is provided by `gradient-worker` processes. The server does not start a worker automatically — at least one must be configured explicitly.

To run a worker on the server host itself, import the `gradient-worker` NixOS module and enable `services.gradient.worker`. For remote build machines or additional capacity:

1. **Register the worker** under an organization (`worker_id` must be a UUID v4):

    ```sh
    curl -X POST https://gradient.example.com/api/v1/orgs/myorg/workers \
      -H "Authorization: Bearer $TOKEN" \
      -H "Content-Type: application/json" \
      -d '{"worker_id": "550e8400-e29b-41d4-a716-446655440001"}'
    # → {"error":false,"message":{"peer_id":"<uuid>","token":"<secret>"}}
    ```

    Store the returned `peer_id` and `token` — the token is shown only once. You can also supply your own pre-generated token via the `token` field (`openssl rand -base64 48`); in that case the response omits the token.

2. **Configure the worker** on the remote machine (see [Configuration → Workers](../configuration.md#workers) for the full NixOS module):

    ```sh
    # Write peers file — one peer_id:token per line.
    # Use * as peer_id to respond with that token for any org UUID.
    echo "<peer_id>:<token>" > /run/secrets/gradient-worker-peers
    ```

3. The worker connects and is visible in **Organization → Workers** in the UI and via `GET /api/v1/orgs/{org}/workers`.

Workers authenticate using per-organization tokens. A worker authorized for an org receives only that org's job offers. Workers with no peers file run in **open mode** and are trusted for all jobs — suitable for co-located workers on a trusted host.
