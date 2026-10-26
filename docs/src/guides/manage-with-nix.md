<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# Manage Gradient with Nix

A Nix setup can declare users, projects, tasks and caches under `services.gradient.state`. A good starting point is a setup created in the UI. See [Declarative State](../concepts/declarative-state.md) for how declared and UI-created entities coexist.

**Requirements:**

- A running instance, see [Quick Start](../get-started/quick-start.md)
- An API key of a superuser account, from **Settings -> API Keys**

## 1. Export the Running State

```sh
curl -H "Authorization: Bearer $TOKEN" \
  https://gradient.example.com/api/v1/admin/state > state.nix
```

The file `state.nix` will hold every user, project, task, cache, custom role, API key, worker and integration. Its shape is that of `services.gradient.state`. Built-in roles, the GitHub App's integrations and the internal `build-request` task are left out.

## 2. Fill In the Secrets

The export stores secrets in hashed or encrypted form. Every `*_file` field comes back as `null` and needs to point at a secret file on the server.

| Field | Content | Generate |
|---|---|---|
| `users.<name>.password_file` | Argon2id hash | `gradient hash`, or `null` for OIDC-only accounts |
| `projects.<name>.private_key_file` | SSH key for cloning | `ssh-keygen -t ed25519 -N "" -f key` |
| `caches.<name>.signing_key_file` | Nix signing key, without the `<name>:` prefix | `nix-store --generate-binary-cache-key` |
| `api_keys.<name>.key_file` | SHA-256 hex digest of the token, without `GRAD` | See [API Key Files](../concepts/declarative-state.md#api-key-files) |
| `integrations.<name>.secret_file` | Webhook secret shared with the Git host | `openssl rand -hex 32` |
| `integrations.<name>.access_token_file` | Git host access token | From the Git host |
| `tasks.<name>.actions.*.config.token_file` | Web request token | `openssl rand -hex 32` |

!!! tip "Superuser Through OIDC"
    On the first OIDC sign-in with a matching username or email, a user with `superuser = true` and no `password_file` becomes the superuser.

## 3. Apply

```nix
services.gradient.state = import ./state.nix;
```

The NixOS build runs `state.validate` on the state. This check is on by default. Unknown users, unknown projects and broken references cause a build failure, not a failure at server start.

!!! warning
    `state.delete` is on by default. Once users, projects or caches disappear from the configuration, Gradient removes them from the database.

## Verify Deployment

- `journalctl -u gradient-server` shows `State configuration validated successfully` after the new state loads.
- Declared entities appear in the UI with their form fields disabled and the hint **Managed by Nix - edit via declarative config**.

## Next Steps

- [State reference](../reference/state.md) - all options, each with an example
- [Add a Remote Worker](remote-worker.md) - declare workers alongside their projects
- [Set Up Single Sign-On](sso.md) - map OIDC and SCIM groups to roles
