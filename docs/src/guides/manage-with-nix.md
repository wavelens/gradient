<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# Manage Gradient with Nix

A Nix setup can declare users, projects, tasks and caches under `services.gradient.state`. A good starting point is a setup created in the UI. See [Declarative State](../concepts/declarative-state.md) for how declared and UI-created entities coexist.

**Requirements:**

- A running instance, see [Quick Start](../get-started/quick-start.md)
- A superuser's API key from **Settings -> API Keys** with every permission, including `readSuperuser` and `writeSuperuser`, and no pin to a project or cache.

## 1. Export the Running State

```sh
curl -H "Authorization: Bearer $TOKEN" \
  https://gradient.example.com/api/v1/admin/state > state.nix
```

File `state.nix` holds the users on record in the state file. This file also holds projects, tasks, caches, custom roles, API keys, workers and all connections to Git hosts. The format is the same as `services.gradient.state`. Accounts from registration, SCIM, or external sign-in don't appear in the export. References to those accounts stay blank. Blank `created_by` values retain the creator that the database already saved at apply time for that project, task, cache, or connection to a Git host. Those accounts don't carry over after import of this file. Built-in roles and internal `build-request` tasks don't appear in the export. API keys that pin a cache don't appear either. The state format doesn't save cache pins for keys.

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
    A user with `superuser = true` and no `password_file` can become a superuser on first OIDC sign-in. That sign-in can claim the account only with a verified email address (`email_verified` claim) from the provider and a match to the declared `email`. A matching username alone can't claim the account.

## 3. Apply

```nix
services.gradient.state = import ./state.nix;
```

The NixOS build runs `state.validate` on the state. This check is on by default. Unknown users, unknown projects and broken references cause a build failure, not a failure at server start.

!!! warning
    `state.delete` is on by default. Once users, projects or caches disappear from the configuration, Gradient removes them from the database.

!!! note
    Upgrade from a release without superuser permissions for API keys. `services.gradient.state` API keys that relied on enumerating all permissions lose superuser access on the next startup. Add `readSuperuser` and `writeSuperuser` to their `permissions`. Logs on the server contain a warning for each such key.

!!! note
    Upgrade from a release without `openJobTerminal`. The server applies the declared `permissions` for roles and API keys as described on every start. Declared roles and keys with `triggerEvaluation` lose job terminals and SSH logins to jobs. Add `openJobTerminal` to their `permissions` to keep that access.

## Verify Deployment

- `journalctl -u gradient-server` shows `State configuration validated successfully` after the new state loads.
- Declared entities show in the UI with disabled form fields and the hint **This item is managed by the server configuration and cannot be changed here.**

## Next Steps

- [State reference](../reference/state.md) - all options, each with an example
- [Add a Remote Worker](remote-worker.md) - declare workers alongside their projects
- [Set Up Single Sign-On](sso.md) - map OIDC and SCIM groups to roles
