# Declarative State

The options below `services.gradient.state` come from `nix/modules`. To work through the setup, read [Manage Gradient with Nix](../guides/manage-with-nix.md). For the model behind this approach, see [Declarative State](../concepts/declarative-state.md).

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
| `api_keys` | attrs of submodule | `{ }` | API keys, one entry per key name. |
| `caches` | attrs of submodule | `{ }` | Caches, one entry per cache name. |
| `delete` | bool | `true` | Deletion of users, projects and caches absent from this declaration. |
| `integrations` | attrs of submodule | `{ }` | Git host integrations, one entry per project name. |
| `projects` | attrs of submodule | `{ }` | Projects, one entry per project name. |
| `roles` | attrs of submodule | `{ }` | Custom roles, one entry per role name. |
| `tasks` | attrs of submodule | `{ }` | Tasks, one entry per task name. |
| `teams` | attrs of submodule | `{ }` | Teams, one entry per team name. |
| `users` | attrs of submodule | `{ }` | Users, one entry per user name. |
| `validate` | bool | `true` | Validation of the generated state at build time through the server's `--state-validate`. Schema and reference errors then fail the Nix build instead of the first server start. |
| `workers` | attrs of submodule | `{ }` | Worker registrations, one entry per worker ID. |

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
| `created_by` | string | - | Name of the user who created the project. |
| `description` | null or string | `null` | Free-text description of the project. |
| `display_name` | string | `name` | Name shown for the project in the UI. |
| `hide_build_requests` | bool | `false` | Set to `true` to keep the project's automatic `build-request` task out of task listings in the web UI. |
| `id` | null or string | `null` | UUID of the project. |
| `members` | list of submodule | `[ ]` | Users that hold a role on the project. |
| `members.*.role` | string | - | Role assigned to the user: one of the built-in `Admin`, `Write` or `View`, or a custom role declared for this project in `state.roles`. |
| `members.*.user` | string | - | Name of the user to add as a member. |
| `name` | string | attribute name | Unique name of the project. |
| `private_key_file` | string | - | Path to the SSH private key used for Git access. |
| `public` | bool | `false` | Set to `true` to make the project visible to all users. |
| `exec_enabled` | bool | `false` | Set to `true` to start exec jobs of the project. Exec entry points of a project without it fail with a message. |
| `teams` | list of submodule | `[ ]` | Teams with access to the project. Team grants declared here and later removed from the list get cleaned up on the next state apply. Grants created through the API or by new-project settings remain untouched. |
| `teams.*.role` | null or string | `null` | Role given to the team's users: one of the built-in `Admin`, `Write` or `View`, or a custom role of this project. Required when `users` is `true`. |
| `teams.*.team` | string | - | Name of the team to grant on the project. |
| `teams.*.users` | bool | `true` | Set to `true` to give the team's users `role` on the project. |
| `teams.*.workers` | bool | `true` | Set to `true` to let the team's workers pick up jobs from the project. |

## `tasks.<name>`

| Option | Type | Default | Description |
|---|---|---|---|
| `actions` | list of submodule | `[ ]` | The actions of this task: email notifications, web requests, Matrix and Slack messages, Git host status reports and pull request automation. |
| `actions.*.active` | bool | `true` | Whether this action is active. |
| `actions.*.config` | attribute set | `{ }` | Configuration specific to the action type. |
| `actions.*.events` | list of string | `[ ]` | Events this action subscribes to. |
| `actions.*.name` | string | - | Action name, unique within the task. |
| `actions.*.type` | one of `send_mail` `send_web_request` `send_matrix_message` `send_slack_message` `git_host_status_report` `open_pr` | - | The kind of action, which determines the expected `config`. |
| `active` | bool | `true` | Whether this task is active. |
| `concurrency` | one of `auto` `hard_abort` `soft_abort` `skip` `all` | `"auto"` | What happens when a new trigger event arrives while an evaluation of the same branch is still running, see [concurrency](../concepts/projects-and-tasks.md#concurrency). |
| `created_by` | string | - | User name of the task's creator. |
| `description` | null or string | `null` | Description of the task. |
| `display_name` | string | `name` | Name of the task shown in the UI. |
| `flake_input_overrides` | attrs of submodule | `{ }` | Overrides applied when fetching flake inputs, with one entry per input name. |
| `flake_input_overrides.<name>.keep_url` | bool | `false` | Whether to force-update this input from its flake-declared URL. |
| `flake_input_overrides.<name>.url` | null or string | `null` | Flake reference that replaces this input. |
| `keep_evaluations` | int | `30` | How many finished evaluations, regardless of outcome, to keep for metrics and history. Must be at least 1 and at most `eval.maxKeep`. |
| `multi_node_evaluation` | bool | `false` | Whether more than one eval worker may evaluate one evaluation of this task together. The server can split an evaluation only when past evaluations predict a gain. |
| `name` | string | attribute name | Unique name of the task. |
| `project` | string | - | Name of the project that owns this task. |
| `repository` | string | - | Git repository URL of the task. |
| `retry_failed_builds` | bool | `true` | Whether a new evaluation of this task re-queues its permanently failed builds and their dependents. With `false`, they remain failed until a user retries the build or the evaluation. |
| `sign_cache` | bool | `true` | Whether to sign the narinfo of outputs pushed by this task. |
| `wait_for_workers` | bool | `false` | Whether an evaluation waits for a worker when its builds need an architecture or system features that no connected worker provides. Without this, the server aborts such an evaluation with a warning naming what is missing. |
| `triggers` | null or list of submodule | `null` | Evaluation triggers of the task: polling, Git host push, Git host pull request or cron schedule. |
| `triggers.*.active` | bool | `true` | Whether this trigger is active. |
| `triggers.*.config` | attribute set | `{ }` | Configuration specific to the trigger type. |
| `triggers.*.integration` | null or string | `null` | Name of an inbound integration in the same project that backs this trigger. |
| `triggers.*.is_default` | bool | `false` | Whether the trigger is the default trigger of the task. The first trigger is the default when no trigger sets the flag. |
| `triggers.*.type` | one of `polling` `reporter_push` `reporter_pull_request` `time` | - | The kind of trigger, which determines the expected `config` and how the trigger fires. |
| `wildcard` | string | `"packages.x86_64-linux.**"` | Comma-separated Nix attribute paths to evaluate from the flake. A `*` segment matches any attribute name, `**` also matches a nested set below it, and `x86_64-*` matches part of a name. A pattern prefixed with `!` excludes matching paths. See [Evaluation Wildcards](wildcards.md). |

## `integrations.<name>`

| Option | Type | Default | Description |
|---|---|---|---|
| `access_token_file` | null or path | `null` | Path to a file with the API token for the Git host, required for outbound integrations. |
| `account_login` | null or string | `null` | GitHub login of the installation account. Used for naming only. |
| `created_by` | string | - | Name of the user who created the integration. |
| `display_name` | null or string | `null` | Human-readable name shown for the integration. |
| `endpoint_url` | null or string | `null` | Base URL of the Git host API for outbound integrations, for example `https://gitea.example.com`. |
| `git_host_type` | one of `gitea` `forgejo` `gitlab` `github` | - | Git host this integration connects to. |
| `installation_id` | null or int | `null` | GitHub App installation ID. This number appears at the end of the installation URL. |
| `kind` | one of `inbound` `outbound` | - | `inbound` integrations receive webhooks from the Git host, verified with HMAC. `outbound` integrations report CI status back to the Git host. |
| `name` | string | attribute name | Integration name. Unique per project and kind. |
| `project` | string | - | Project that owns this integration. |
| `secret_file` | null or path | `null` | Path to a file with the HMAC signing secret, required for inbound integrations. |

## `caches.<name>`

| Option | Type | Default | Description |
|---|---|---|---|
| `active` | bool | `true` | Cache activity flag. The UI can change this value, and the server restores it on the next start. |
| `created_by` | string | - | Name of the user who created the cache. |
| `description` | null or string | `null` | Cache description. |
| `display_name` | string | `name` | Name shown in the UI for this cache. |
| `local_priority` | null or int | `null` | Priority written to `nix-cache-info` for clients inside `http.localIps`. |
| `max_storage_gb` | int | `0` | Storage limit for the cache, in GB. |
| `members` | list of submodule | `[ ]` | Users with a direct role on this cache. |
| `members.*.role` | string | - | Role assigned to the user: one of the built-in `Admin`, `Write`, `View`, or a custom role defined on this cache. |
| `members.*.user` | string | - | User name, resolved during state application. |
| `name` | string | attribute name | Unique name of the cache. |
| `priority` | int | `10` | Priority written to `nix-cache-info`. Nix queries caches with lower values first. |
| `projects` | list of string | `[ ]` | Names of the projects that use this cache. |
| `pull_through` | bool | `true` | Whether this cache serves paths from its upstream caches to Nix clients. |
| `public` | bool | `false` | Whether every project can use this cache. |
| `teams` | list of submodule | `[ ]` | Teams with a grant on this cache. Grants declared here but removed from the list get revoked on the next state apply. Grants made through the API remain untouched. |
| `teams.*.role` | string | - | Role given to the team's users: `Admin`, `Write`, `View`, or a custom role defined on this cache. |
| `teams.*.team` | string | - | Team that receives the grant. |
| `roles` | list of submodule | `[ ]` | Custom roles defined on this cache. |
| `roles.*.name` | string | - | Name of the custom role, distinct from the built-in roles. |
| `roles.*.permissions` | list of string | `[ ]` | Cache permissions granted by the role: `viewCache`, `readStore`, `writeStore`, `manageCacheSettings`, `manageCacheKeys`, `manageUpstreamCaches`, `manageCacheMembers`, `manageCacheRoles`, `manageCacheSubscriptions`, `manageCacheWebhooks` or `deleteCache`. |
| `signing_key_file` | string | - | Path to the file with the Nix cache signing key. |
| `upstream_caches` | list of submodule | cache.nixos.org | Upstream caches used as substituters: internal Gradient caches or external Nix binary caches. |
| `upstream_caches.*.active` | boolean | `true` | Whether the upstream cache is in use. Gradient keeps inactive upstream caches but never queries them. The UI can change this value, and the server restores it on the next start. |
| `upstream_caches.*.cache_name` | null or string | `null` | Name of the internal Gradient cache to use. |
| `upstream_caches.*.display_name` | null or string | `null` | Name shown in the UI for the upstream cache. |
| `upstream_caches.*.http1_only` | boolean | `false` | Whether requests to the external Nix binary cache use HTTP/1.1 only. Enable this for caches that break HTTP/2 transfers. |
| `upstream_caches.*.mode` | one of `ReadWrite` `ReadOnly` `WriteOnly` | `"ReadWrite"` | Access mode for an internal upstream cache. |
| `upstream_caches.*.public_key` | null or string | `null` | Public key of the external Nix binary cache. Required for `external` upstream caches. |
| `upstream_caches.*.type` | one of `internal` `external` | - | Kind of upstream: `internal` for another Gradient cache, `external` for a Nix binary cache URL. |
| `upstream_caches.*.url` | null or string | `null` | URL of the external Nix binary cache. |

## `roles.<name>`

| Option | Type | Default | Description |
|---|---|---|---|
| `name` | string | attribute name | Role name. Unique within the project and distinct from the built-in roles `Admin`, `Write` and `View`. |
| `permissions` | list of string | `[ ]` | Permissions granted by the role. Each entry is a camelCase identifier. |
| `project` | string | - | Project that owns the role. |

## `api_keys.<name>`

| Option | Type | Default | Description |
|---|---|---|---|
| `key_file` | string | - | Path to a file that hold the SHA-256 hash of the API token. The hash use lowercase hex and omit the `GRAD` prefix. |
| `name` | string | attribute name | Name of this API key. |
| `owned_by` | string | - | Name of the user who own this key. |
| `permissions` | list of string | `[ ]` | Permissions this key grant, written as camelCase identifiers such as `viewProject`, `triggerEvaluation`, `editTask` or `manageMembers`. |
| `project` | null or string | `null` | Project to which this key restrict access. `null` mean no restriction. |

## `teams.<name>`

| Option | Type | Default | Description |
|---|---|---|---|
| `display_name` | string | attribute name | Name of the team shown in the UI. |
| `members` | list of submodule | `[ ]` | Users belonging to the team. A member declared here but removed from the list leaves the team on the next state apply. Members added through the API, an OIDC group or a SCIM group remain untouched. |
| `members.*.role` | one of `Admin` `Member` | `"Member"` | Role of the member in the team. Admins can manage members, workers, grants and requests. |
| `members.*.user` | string | - | Name of the user, resolved at state apply time. |
| `name` | string | attribute name | Name of the team. A team managed here has its settings locked in the API, while other members and workers can still join through the API. |
| `new_projects.role` | null or one of `Admin` `Write` `View` | `null` | Project role given to the team's users on new projects. |
| `new_projects.users` | bool | `false` | Grants the team's users `new_projects.role` on new projects when enabled. |
| `new_projects.workers` | bool | `false` | Grants the team's workers on new projects when enabled. |
| `oidc_group` | null or string | `null` | OIDC group whose members join the team at sign-in. They leave the team once the group no longer appears in their `groups` claim. The `groups` scope must be enabled. |
| `scim_group` | null or string | `null` | SCIM group mapped to the team's members. |

## `workers.<name>`

Each worker registration has the following options.

| Option | Type | Default | Description |
|---|---|---|---|
| `created_by` | null or string | `null` | Name of the user who created the registration. |
| `display_name` | string | attribute name | Name shown for the worker in the UI. |
| `enable_build` | bool | `true` | Grant of the worker's `build` capability by the server. |
| `enable_eval` | bool | `true` | Grant of the worker's `eval` capability by the server. |
| `enable_exec` | bool | `false` | Grant of the worker's `exec` capability by the server. |
| `enable_network` | bool | `true` | Grant of the worker's `network` capability by the server. |
| `enable_secrets` | bool | `true` | Grant of the worker's `secrets` capability by the server. |
| `enabled` | bool | `true` | Activation state of the worker. UI changes persist only until the next server start. |
| `projects` | list of string | `[ ]` | Projects under which the worker registered, one registration per project. An empty list marks a team worker. |
| `team` | null or string | `null` | Owning team. A team worker can build for every project that grants the team's workers. Cannot be combined with `projects`. |
| `token_file` | path | - | File with the worker's authentication token. |
| `url` | null or string | `null` | WebSocket URL where the worker accepts server connections. |
| `worker_id` | string | - | Identity of the worker. |

## Trigger Types

| `type` | `integration` | `config` |
|---|---|---|
| `polling` | - | `interval_secs` (minimum 10, default 300) and `branch` (default: the remote HEAD) |
| `reporter_push` | Inbound integration | `branches`, `tags` (globs, an empty value matches all), `releases_only` |
| `reporter_pull_request` | Inbound integration | `branches`, `actions` (default `opened`, `synchronize`, `reopened`), `require_approval` (default `true`: pull requests from non-writers wait for a maintainer's approval) |
| `time` | - | `cron` with six UTC fields, `sec min hour dom mon dow`, for example `"0 0 2 * * *"` |

`triggers = null` leaves the current triggers in place. New tasks without a `triggers` key start with a polling trigger as the default. An empty list or two default triggers count as invalid configuration.

## Action Types

| `type` | `events` | `config` |
|---|---|---|
| `send_mail` | Required | `recipients`, `subject_template` |
| `send_web_request` | Required | `url`, `token_file` |
| `send_matrix_message` | Required | `homeserver`, `room_id`, `access_token_file` |
| `send_slack_message` | Required | `webhook_url_file` |
| `git_host_status_report` | Empty | `integration`: an outbound integration |
| `open_pr` | Empty | `integration`, `generator`, `granularity`, `verify_gate`, `branch_pattern`, `title_template`, `body_template`, `update_existing`, see [Update Flake Inputs](../guides/flake-updates.md) |

Event names live in the [events reference](events.md).
