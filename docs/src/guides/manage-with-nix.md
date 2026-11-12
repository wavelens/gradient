<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# Manage Gradient with Nix

A Nix setup can declare users, projects, tasks and caches under `services.gradient.state`. A good starting point is a setup created in the UI. See [Declarative State](../concepts/declarative-state.md) for how declared and UI-created entities coexist.

**Requirements:**

- A running instance, see [Quick Start](../get-started/quick-start.md)
- A superuser's API key from **Settings -> API Keys** with every permission, including `useSuperuserRights`, and no pin to a project or cache.

## 1. Export the Running State

```sh
curl -H "Authorization: Bearer $TOKEN" \
  https://gradient.example.com/api/v1/admin/state > state.nix
```

All users, projects, tasks, caches, custom roles, API keys, workers and connections to Git hosts sit in file `state.nix`. The format is identical to `services.gradient.state`. Built-in roles and internal `build-request` tasks don't appear in the export. API keys that pin a cache don't appear either. The state format doesn't store a cache pin for keys.

## 2. Fill In the Secrets

The export stores secrets in hashed or encrypted form. Every `*_file` field comes back as `null` and needs to point at a secret file on the server.

| Field | Content | Generate |
|---|---|---|
| `users.<name>.password_file` | Argon2id hash | `gradient hash`, or `null` for OIDC-only accounts |
| `projects.<name>.private_key_file` | SSH key for cloning | `ssh-keygen -t ed25519 -N "" -f key` |
| `caches.<name>.signing_key_file` | Nix signing key, without the `<name>:` prefix | `nix-store --generate-binary-cache-key` |
| `api_keys.<name>.key_file` | SHA-256 hex digest of the token, without `GRAD` | See [API Key Files](../concepts/declarative-state.md#api-key-files) |
| `git_host_connections.<name>.secret_file` | Webhook secret shared with the Git host | `openssl rand -hex 32` |
| `git_host_connections.<name>.access_token_file` | Git host access token | From the Git host |
| `tasks.<name>.actions.*.config.token_file` | Web request token | `openssl rand -hex 32` |

!!! tip "Superuser Through OIDC"
    Users with `superuser = true` and without `password_file` become superusers at the first OIDC sign-in. The sign-in can match a username or a verified email address (the `email_verified` claim).

## 3. Apply

```nix
services.gradient.state = import ./state.nix;
```

The NixOS build runs `state.validate` on the state. This check is on by default. Unknown users, unknown projects and broken references cause a build failure, not a failure at server start.

!!! warning
    `state.delete` is on by default. Once users, projects or caches disappear from the configuration, Gradient removes them from the database.

!!! note
    Upgrading from a release without `useSuperuserRights`: API keys from `services.gradient.state` that relied on listing every permission lose superuser access on the next start. Add `useSuperuserRights` to their `permissions`. The server logs a warning for each such key.

## Verify Deployment

- `journalctl -u gradient-server` shows `State configuration validated successfully` after the new state loads.
- Declared entities show in the UI with disabled form fields and the hint **This item is managed by the server configuration and cannot be changed here.**

## Next Steps

- [State reference](../reference/state.md) - all options, each with an example
- [Add a Remote Worker](remote-worker.md) - declare workers alongside their projects
- [Set Up Single Sign-On](sso.md) - map OIDC and SCIM groups to roles
