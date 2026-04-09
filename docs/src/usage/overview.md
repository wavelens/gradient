# Overview

## Getting Started

1. **Register / Log in** — `/` redirects to login automatically.
2. **Create an organization** — organizations own servers, projects, and caches.
3. **Add a build server** — open the organization, go to **Servers**, and add the server. Then add the organization's SSH public key to the server's `authorized_keys` (see [SSH Keys](#ssh-keys) below).
4. **Create a project** — point it at a Git repository and set an evaluation wildcard.

## Evaluation Wildcard

The wildcard is a dot-separated Nix attribute path selecting which flake outputs to build. Multiple paths are separated by commas (no spaces). The root level is restricted to: `checks`, `packages`, `formatter`, `legacyPackages`, `nixosConfigurations`, `devShells`, `hydraJobs`.

Two special tokens control expansion at each level:

| Token | Behaviour |
|---|---|
| `*` | Expand all keys at this level without type-checking (recursive — children are processed further) |
| `#` | Type-check at this level: only attributes where `type == "derivation"` are built (non-recursive) |

An implicit `.#` is appended to every wildcard, so `packages.x86_64-linux.*` becomes `packages.x86_64-linux.*.#` internally.

Consecutive `*` segments collapse into one — `packages.*.*` and `packages.*` are equivalent.

### Exclusions

Prefix a pattern with `!` to exclude matching paths from the set produced by the preceding include patterns. Exclusions are evaluated in order — each `!`-prefixed pattern removes anything it matches from the accumulated set.

```
packages.*,!packages.x86_64-linux.broken
```

The above builds all packages on all systems except `packages.x86_64-linux.broken`.

Exclusion patterns must be exact paths — they cannot contain `*` or `#`.

| Wildcard | Builds |
|---|---|
| `packages.x86_64-linux.#` | All x86\_64-linux packages |
| `checks.x86_64-linux.*` | All x86\_64-linux checks |
| `packages.*` | Packages for all systems |
| `packages.x86_64-linux.*,checks.x86_64-linux.*` | Both |
| `nixosConfigurations.*.config.system.build.toplevel` | All NixOS configurations |
| `packages.*,!packages.x86_64-linux.broken` | All packages except one excluded path |

## Evaluations

Click **Start Evaluation** on the project page. Gradient clones the repo, evaluates each wildcard match, and dispatches the resulting derivations to the configured build servers.

The evaluation log page shows per-build status, combined ANSI build output, and an **Abort** button.

## SSH Keys

Each organization has one Ed25519 SSH key pair, generated automatically. The public key is shown in **Organization → Settings → SSH**.

Add this key to:

- **Build servers** — in `authorized_keys` so Gradient can connect and run builds.
- **Git hosts** — as a deploy key if your repository is cloned over SSH.

The key is scoped to the organization; different organizations use different keys.
