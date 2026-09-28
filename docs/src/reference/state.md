# Declarative State

Every option under `services.gradient.state`, generated from `nix/modules`. For the workflow, see [Manage Gradient with Nix](../guides/manage-with-nix.md); for the model, [Declarative State](../concepts/declarative-state.md).

```nix
services.gradient.state = {
  users.alice = { email = "alice@example.com"; password_file = "/run/secrets/alice-password"; };
  projects.acme = { created_by = "alice"; private_key_file = "/run/secrets/acme-ssh-key"; };
  caches.main = { created_by = "alice"; signing_key_file = "/run/secrets/main-cache-key"; projects = [ "acme" ]; };
  tasks.web-app = { project = "acme"; created_by = "alice"; repository = "https://github.com/acme/web-app"; };
};
```

## General

| Option | Type | Default | Description |
|---|---|---|---|
| `api_keys` | attrs of submodule | `{ }` | API keys to create, keyed by name. |
| `caches` | attrs of submodule | `{ }` | Caches to create, keyed by name. |
| `delete` | bool | `true` | Whether to delete users, projects and caches no longer declared here. |
| `integrations` | attrs of submodule | `{ }` | Forge integrations per project, keyed by name. |
| `projects` | attrs of submodule | `{ }` | Projects to create, keyed by name. |
| `roles` | attrs of submodule | `{ }` | Custom roles, keyed by role name. |
| `tasks` | attrs of submodule | `{ }` | Tasks to create, keyed by name. |
| `users` | attrs of submodule | `{ }` | Users to create, keyed by user name. |
| `validate` | bool | `true` | Whether to validate the generated state at build time with the server's `--state-validate`. Schema and reference errors then fail the Nix build instead of the first server start. |
| `workers` | attrs of submodule | `{ }` | Worker registrations, keyed by worker ID. |

## `users.<name>`

| Option | Type | Default | Description |
|---|---|---|---|
| `email` | string | - | Email address of the user. |
| `email_verified` | bool | `true` | Whether the user's email address is verified. |
| `name` | string | `username` | Full name of the user. |
| `password_file` | null or string | `null` | File containing the hashed password. |
| `superuser` | bool | `false` | Whether the user is a superuser. |
| `username` | string | attribute name | Unique user name. |

## `projects.<name>`

| Option | Type | Default | Description |
|---|---|---|---|
| `created_by` | string | - | User name of the project's creator. |
| `description` | null or string | `null` | Description of the project. |
| `display_name` | string | `name` | Display name of the project. |
| `hide_build_requests` | bool | `false` | Whether to hide the project's automatic `build-request` task from task listings in the web UI. |
| `id` | null or string | `null` | Project UUID. |
| `members` | list of submodule | `[ ]` | Users with roles on this project. |
| `members.*.role` | string | - | Role name: a built-in `Admin`, `Write` or `View`, or a custom role of the same project declared in `state.roles`. |
| `members.*.user` | string | - | User name to grant membership to. |
| `name` | string | attribute name | Unique project name. |
| `private_key_file` | string | - | File containing the SSH private key used for Git access. |
| `public` | bool | `false` | Whether the project is visible to all users. |

## `tasks.<name>`

| Option | Type | Default | Description |
|---|---|---|---|
| `actions` | list of submodule | `[ ]` | Task actions: email notifications, web requests, forge status reports and pull request automation. |
| `actions.*.active` | bool | `true` | Whether the action is active. |
| `actions.*.config` | attribute set | `{ }` | Type-specific configuration. |
| `actions.*.events` | list of string | `[ ]` | Events the action subscribes to. |
| `actions.*.name` | string | - | Action name, unique within the task. |
| `actions.*.type` | one of `send_mail` `send_web_request` `forge_status_report` `open_pr` | - | Action kind, which determines the expected `config`. |
| `active` | bool | `true` | Whether the task is active. |
| `concurrency` | one of `hard_abort` `soft_abort` `skip` `all` | `"soft_abort"` | What a new trigger event does while an evaluation is running. |
| `created_by` | string | - | User name of the task's creator. |
| `description` | null or string | `null` | Description of the task. |
| `display_name` | string | `name` | Display name of the task. |
| `flake_input_overrides` | attrs of submodule | `{ }` | Overrides applied when fetching flake inputs, keyed by input name. |
| `flake_input_overrides.<name>.keep_url` | bool | `false` | Whether to force-update this input from its flake-declared URL. |
| `flake_input_overrides.<name>.url` | null or string | `null` | Flake reference overriding this input. |
| `keep_evaluations` | int | `1` | Number of finished evaluations kept for metrics and history, regardless of outcome. Tasks created in the UI or API keep 30. |
| `name` | string | attribute name | Unique task name. |
| `project` | string | - | Name of the project the task belongs to. |
| `repository` | string | - | Git repository URL of the task. |
| `sign_cache` | bool | `true` | Whether to sign the narinfo of outputs pushed by this task. |
| `triggers` | null or list of submodule | `null` | Evaluation triggers of the task: polling, forge push, forge pull request or cron schedule. |
| `triggers.*.active` | bool | `true` | Whether the trigger is active. |
| `triggers.*.config` | attribute set | `{ }` | Type-specific configuration. |
| `triggers.*.integration` | null or string | `null` | Name of an inbound integration in the same project backing this trigger. |
| `triggers.*.type` | one of `polling` `reporter_push` `reporter_pull_request` `time` | - | Trigger kind, which determines the expected `config` and how the trigger fires. |
| `wildcard` | string | `"packages.x86_64-linux.*"` | Attribute paths to evaluate, as [wildcards](wildcards.md); `packages.x86_64-linux.#` is recommended. |

## `integrations.<name>`

| Option | Type | Default | Description |
|---|---|---|---|
| `access_token_file` | null or path | `null` | File containing the forge API token of an outbound integration. |
| `account_login` | null or string | `null` | GitHub account login of the installation, used for naming only. |
| `created_by` | string | - | User name of the integration's creator. |
| `display_name` | null or string | `null` | Display name of the integration. |
| `endpoint_url` | null or string | `null` | Base URL of the forge API for outbound integrations, such as `https://gitea.example.com`. |
| `forge_type` | one of `gitea` `forgejo` `gitlab` `github` | - | Forge this integration targets. |
| `installation_id` | null or int | `null` | GitHub App installation ID, the trailing number of the installation URL. |
| `kind` | one of `inbound` `outbound` | - | Direction of the integration: `inbound` for HMAC-verified webhooks from the forge, `outbound` for CI status reports to the forge. |
| `name` | string | attribute name | Integration name, unique per project and kind. |
| `project` | string | - | Name of the project the integration belongs to. |
| `secret_file` | null or path | `null` | File containing the HMAC signing secret of an inbound integration. |

## `caches.<name>`

| Option | Type | Default | Description |
|---|---|---|---|
| `active` | bool | `true` | Whether the cache is active. |
| `created_by` | string | - | User name of the cache's creator. |
| `description` | null or string | `null` | Description of the cache. |
| `display_name` | string | `name` | Display name of the cache. |
| `local_priority` | null or int | `null` | Priority advertised in `nix-cache-info` to clients within `http.localIps`. |
| `max_storage_gb` | int | `0` | Storage limit of the cache in GB. |
| `members` | list of submodule | `[ ]` | Users with direct roles on this cache. |
| `members.*.role` | string | - | Role name: a built-in `Admin`, `Write` or `View`, or a custom role of this cache. |
| `members.*.user` | string | - | User name, resolved when the state is applied. |
| `name` | string | attribute name | Unique cache name. |
| `priority` | int | `10` | Priority of the cache; lower is preferred, as in Nix. |
| `projects` | list of string | `[ ]` | Names of the projects using this cache. |
| `public` | bool | `false` | Whether the cache is available to all projects. |
| `roles` | list of submodule | `[ ]` | Custom roles of this cache. |
| `roles.*.name` | string | - | Custom role name, distinct from the built-in roles. |
| `roles.*.permissions` | list of string | `[ ]` | Cache permissions granted by the role: `viewCache`, `readStore`, `writeStore`, `manageCacheSettings`, `manageCacheKeys`, `manageCacheUpstreams`, `manageCacheMembers`, `manageCacheRoles`, `manageCacheSubscriptions`, `manageCacheWebhooks` or `deleteCache`. |
| `signing_key_file` | string | - | File containing the Nix cache signing key. |
| `upstreams` | list of submodule | cache.nixos.org | Upstream caches used as substituters: internal Gradient caches or external Nix binary caches. |
| `upstreams.*.cache_name` | null or string | `null` | Name of the internal Gradient cache to use. |
| `upstreams.*.display_name` | null or string | `null` | Display name of the upstream. |
| `upstreams.*.mode` | one of `ReadWrite` `ReadOnly` `WriteOnly` | `"ReadWrite"` | Access mode of an internal upstream. |
| `upstreams.*.public_key` | null or string | `null` | Public key of the external Nix binary cache. |
| `upstreams.*.type` | one of `internal` `external` | - | Upstream type: `internal` (another Gradient cache) or `external` (a Nix binary cache URL). |
| `upstreams.*.url` | null or string | `null` | URL of the external Nix binary cache. |

## `roles.<name>`

| Option | Type | Default | Description |
|---|---|---|---|
| `name` | string | attribute name | Role name, distinct from the built-in `Admin`, `Write` and `View` and unique within its project. |
| `oidc_group` | list of string | `[ ]` | OIDC groups granting this role on login. |
| `permissions` | list of string | `[ ]` | Permissions granted by the role, as camelCase identifiers. |
| `project` | string | - | Project the role belongs to. |
| `scim_group` | list of string | `[ ]` | SCIM groups granting this role. |

## `api_keys.<name>`

| Option | Type | Default | Description |
|---|---|---|---|
| `key_file` | string | - | File containing the lowercase hex SHA-256 digest of the API token, without its `GRAD` prefix. |
| `name` | string | attribute name | Name of the API key. |
| `owned_by` | string | - | User name of the key's owner. |
| `permissions` | list of string | `[ ]` | Permissions granted by the key, as camelCase identifiers such as `viewProject`, `triggerEvaluation`, `editTask` or `manageMembers`. |
| `project` | null or string | `null` | Project the key is restricted to. |

## `workers.<name>`

| Option | Type | Default | Description |
|---|---|---|---|
| `authorize_against` | null or string | `null` | UUID a base worker authenticates as instead of the per-project challenge. |
| `auto_enable` | bool | `true` | Whether every new project enables this base worker on creation instead of opting in through the web UI. |
| `base_worker` | bool | `true` | Whether this is a base worker available to every project instead of a per-project registration. |
| `created_by` | null or string | `null` | User name of the registration's creator. |
| `display_name` | string | attribute name | Display name of the worker. |
| `enable_build` | bool | `true` | Whether the server grants this registration the worker's `build` capability. |
| `enable_eval` | bool | `true` | Whether the server grants this registration the worker's `eval` capability. |
| `enable_fetch` | bool | `true` | Whether the server grants this registration the worker's `fetch` capability. |
| `enabled` | bool | `true` | Whether the base worker is available at all. |
| `projects` | list of string | `[ ]` | Projects the worker is registered under, one registration per project; a single worker can serve several projects. |
| `token_file` | path | - | File containing the worker's authentication token. |
| `url` | null or string | `null` | WebSocket URL on which the worker accepts server connections. |
| `worker_id` | string | - | Worker identity. |

## Trigger Types

| `type` | `integration` | `config` |
|---|---|---|
| `polling` | - | `interval_secs` (at least 10, default 300), `branch` (default: the remote HEAD) |
| `reporter_push` | Inbound integration | `branches`, `tags` (globs, empty matches all), `releases_only` |
| `reporter_pull_request` | Inbound integration | `branches`, `actions` (default `opened`, `synchronize`, `reopened`) |
| `time` | - | `cron`, six fields in UTC: `sec min hour dom mon dow`, e.g. `"0 0 2 * * *"` |

`triggers = null` keeps the existing triggers; a new task gets a `polling` trigger every 300 seconds until triggers are declared.

## Action Types

| `type` | `events` | `config` |
|---|---|---|
| `send_mail` | Required | `recipients`, `subject_template` |
| `send_web_request` | Required | `url`, `token_file` |
| `forge_status_report` | Empty | `integration`: an outbound integration |
| `open_pr` | Empty | `integration`, `generator`, `granularity`, `verify_gate`, `branch_pattern`, `title_template`, `body_template`, `update_existing`, see [Update Flake Inputs](../guides/flake-updates.md) |

The event names are in the [events reference](events.md).
