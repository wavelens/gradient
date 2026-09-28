# Manage Gradient with Nix

Users, projects, tasks and caches declared under `services.gradient.state`, starting from a setup built in the UI. See [Declarative State](../concepts/declarative-state.md) for how declared and UI-created entities live side by side.

**Requirements:**

- A running instance, see [Quick Start](../get-started/quick-start.md)
- An API key of a superuser account, from **Settings -> API Keys**

## 1. Export the Running State

```sh
curl -H "Authorization: Bearer $TOKEN" \
  https://gradient.example.com/api/v1/admin/state > state.nix
```

`state.nix` holds every user, project, task, cache, custom role, API key, worker and integration in the shape of `services.gradient.state`. Built-in roles, the GitHub App's integrations and the internal `build-request` task are left out.

## 2. Fill In the Secrets

Gradient stores secrets hashed or encrypted, and the export returns every `*_file` field as `null`. Point each one at a secret on the server:

| Field | Content | Generate |
|---|---|---|
| `users.<name>.password_file` | Argon2id hash | `gradient hash`; `null` for OIDC-only accounts |
| `projects.<name>.private_key_file` | SSH key for cloning | `ssh-keygen -t ed25519 -N "" -f key` |
| `caches.<name>.signing_key_file` | Nix signing key, without the `<name>:` prefix | `nix-store --generate-binary-cache-key` |
| `api_keys.<name>.key_file` | SHA-256 hex digest of the token, without `GRAD` | See [API Key Files](../concepts/declarative-state.md#api-key-files) |
| `integrations.<name>.secret_file` | Webhook secret shared with the forge | `openssl rand -hex 32` |
| `integrations.<name>.access_token_file` | Forge access token | From the forge |
| `tasks.<name>.actions.*.config.token_file` | Web request token | `openssl rand -hex 32` |

!!! tip "Superuser through OIDC"
    A user with `superuser = true` and no `password_file` becomes the superuser on the first OIDC sign-in with a matching username or email.

## 3. Apply

```nix
services.gradient.state = import ./state.nix;
```

The NixOS build checks the state (`state.validate`, on by default): unknown users or projects and broken references fail the build, not the server start.

!!! warning
    `state.delete` is on by default: users, projects and caches later removed from the configuration are deleted from the database.

## Verify Deployment

- `journalctl -u gradient-server` shows `State configuration validated successfully`.
- Declared entities show their form fields disabled in the UI, with the hint **Managed by Nix - edit via declarative config**.

## Next Steps

- [State reference](../reference/state.md): every option with examples
- [Add a Remote Worker](remote-worker.md): workers declared next to their projects
- [Set Up Single Sign-On](sso.md): roles from OIDC and SCIM groups
