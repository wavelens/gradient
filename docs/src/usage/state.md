# Declarative State

`services.gradient.state` lets you declare users, organizations, projects, caches, and API keys in Nix. Gradient reconciles this state on every startup.

When `settings.deleteState = true` (default), entities that are removed from `state` are also deleted from the database. Set it to `false` to make them editable by users in the frontend instead.

## Build-time validation

`services.gradient.validateState` (default `true`) checks the generated state at **build time** by running the server binary's `--validate-state` over it. Schema and cross-reference errors — unknown organizations or users, reporter triggers pointing at an undeclared inbound integration, duplicate org ids, and so on — then fail the Nix build instead of the server on first start. No database is touched and no secret files are required, so it is safe to run in CI. Set it to `false` to skip the check.

To validate a state file by hand:

```sh
gradient-server --state-file ./gradient-state.json --validate-state
```

## State-Managed Resources

Users, organizations, and caches created by the NixOS module configuration carry `managed = true`. The API rejects mutations and deletions of these records with `403 Forbidden`. This allows declarative configuration to be the source of truth without Gradient's UI overwriting it.

### How the UI surfaces this

Managed-resource and read-only access show up consistently across the dashboard:

- **State-managed resources** show form fields and write buttons (Save,
  Delete, etc.) as visible but disabled, with a hover tooltip ("Managed
  by Nix - edit via declarative config") so you can see what the
  resource looks like and what actions exist, you just can't trigger
  them here. Update the Nix config instead.
- **Read-only access** (your organization role doesn't grant write
  permission) shows form fields as disabled with a "You have read-only
  access" tooltip; write buttons are **hidden entirely** - they don't
  exist for you. Contact an organization admin to make changes.

The pages themselves are always navigable. A state-managed cache's
upstreams subpage, for example, is reachable so you can see what
upstreams are configured - only the Add / Edit / Delete controls are
gated.

## Users

```nix
services.gradient.state.users = {
  alice = {
    name           = "Alice";
    email          = "alice@example.com";
    password_file  = "/run/secrets/alice-password";
    email_verified = true;
    superuser      = false;
  };
  # OIDC-only account: no local password. The first OIDC login matching
  # this email/username claims the account.
  bob = {
    name  = "Bob";
    email = "bob@example.com";
    # password_file omitted (defaults to null)
  };
};
```

The password file must contain an **argon2id PHC hash** (a string starting
with `$argon2id$…`). Generate one with the `gradient` CLI:

```sh
# Prompts for the password twice (without echo) and prints the PHC hash
gradient hash > /run/secrets/alice-password
```

You can also use the standalone `argon2` CLI from `libargon2` if you prefer:

```sh
nix shell nixpkgs#libargon2 -c \
  sh -c 'printf %s "mypassword" | argon2 "$(openssl rand -hex 16)" -id -e -m 15 -t 2 -p 1' \
  > /run/secrets/alice-password
```

> **Do not** feed the password via a bash herestring (`<<< "mypassword"`) -
> herestrings append a trailing newline, so `argon2` would hash
> `mypassword\n` and later logins with `mypassword` would fail. Use
> `printf %s` (no `\n`) or `gradient hash`.

At server startup, the file content is validated to start with `$argon2`
and stored verbatim - the server never sees the plaintext password.

Set `password_file = null` (or omit it) for users that authenticate
exclusively via OIDC. Provisioning a user *with* a password and then
attempting to sign in as that user via OIDC is rejected by the server
(`User already exists with password authentication`), so OIDC-only users
must be declared without one.

A *claimable* account is one provisioned without a password that has not yet
been bound to an OIDC identity. On the first OIDC login whose username or email
matches it, the server records its `(iss, sub)` and binds the account;
`superuser` and the other provisioned fields are preserved. This is the
supported way to **bootstrap an OIDC superuser**: declare the user with
`superuser = true` and `password_file = null`, then sign in via OIDC.

### User options

| Option | Default | Description |
|---|---|---|
| `username` | `<attrset key>` | Unique username |
| `name` | `<username>` | Display name |
| `email` | - | Email address (required) |
| `password_file` | `null` | Argon2id PHC hash file. `null` for OIDC-only accounts |
| `email_verified` | `true` | Mark the email as verified at provision time |
| `superuser` | `false` | Grant instance-wide admin |

## Organizations

```nix
services.gradient.state.organizations = {
  acme = {
    display_name     = "ACME Corp";
    description      = "Internal builds for ACME";
    private_key_file = "/run/secrets/acme-ssh-key";
    public           = false;
    created_by       = "alice";
  };
};
```

The SSH private key is the organization's identity key used to clone Git repositories over SSH. Generate one with:

```sh
ssh-keygen -t ed25519 -N "" -f /run/secrets/acme-ssh-key
# Add the public key (.pub) to your Git host as a deploy key
```

To pin the organization UUID for a fully declarative deployment (so a worker's `peersFile` can reference it before the server first starts), generate one with `uuidgen` and set it as `id`:

```sh
uuidgen   # e.g. 018f6f3a-0000-7000-8000-000000000001
```

```nix
services.gradient.state.organizations.acme = {
  id               = "018f6f3a-0000-7000-8000-000000000001";
  display_name     = "ACME Corp";
  private_key_file = "/run/secrets/acme-ssh-key";
  created_by       = "alice";
};
```

### Organization options

| Option | Default | Description |
|---|---|---|
| `name` | `<attrset key>` | Unique organization name |
| `display_name` | `<name>` | Display name |
| `id` | `null` | Explicit organization UUID, applied on create only. Pin it to reference the org in a worker's `peersFile` (`<id>:<token>`) without first looking up the server-generated id. Immutable — a value conflicting with an existing org is rejected |
| `description` | `null` | Optional description |
| `private_key_file` | - | Path to SSH private key (required) |
| `public` | `false` | Visible to all users |
| `github_installation_id` | `null` | GitHub App installation id to bind to this org (look it up on the App's "Install App" page on GitHub). Setting this enables outbound CI status reporting and webhook routing. When `null`, the field is left untouched on update so a webhook-recorded id survives reconciliation |
| `created_by` | - | Username of creator (required) |
| `members` | `[]` | Per-org membership list. When non-empty, the list is authoritative (drift removes unlisted memberships, the implicit creator-Admin step is skipped). Empty preserves the legacy behavior. Members referencing not-yet-registered users are skipped silently and backfilled on registration / OIDC first-login |

### Organization members

Declare per-org membership inline:

```nix
services.gradient.state.organizations.acme = {
  display_name     = "ACME Corp";
  private_key_file = "/run/secrets/acme-ssh-key";
  created_by       = "alice";
  members = [
    { user = "alice"; role = "Admin"; }
    { user = "bob";   role = "Write"; }
    { user = "carol"; role = "releaser"; }   # custom org role from state.roles
  ];
};
```

When `members` is **empty** (the default), the `created_by` user is added as Admin and no other membership reconciliation happens — this is the legacy behavior.

When `members` is **non-empty**, the list is the source of truth:

- Built-in roles (`Admin`, `Write`, `View`) and state-managed custom org roles are both accepted.
- Members referencing **unknown users are skipped silently** at provision time. The membership is applied automatically the instant that user registers (`POST /user`) or first-logs-in via OIDC.
- Memberships no longer in the list are removed on next state apply (drift reconciliation, mirroring cache members).
- The implicit "creator becomes Admin" rule does **not** fire — list yourself explicitly if you want it.

| Option | Default | Description |
|---|---|---|
| `members.*.user` | - | Username (required) |
| `members.*.role` | - | `Admin`/`Write`/`View` or a custom org role declared in `state.roles` for the same organization (required) |

## Projects

```nix
services.gradient.state.projects = {
  web-app = {
    organization         = "acme";
    display_name         = "Web App";
    description          = "Production web application";
    repository           = "git@github.com:acme/web-app.git";
    wildcard             = "packages.x86_64-linux.*";
    active               = true;
    concurrency          = "hard_abort"; # optional, default "soft_abort"
    outbound_integration = "acme-status-reports"; # optional
    created_by           = "alice";
  };
};
```

### Project options

| Option | Default | Description |
|---|---|---|
| `name` | `<attrset key>` | Unique project name |
| `organization` | - | Owning organization (required) |
| `display_name` | `<name>` | Display name |
| `description` | `null` | Optional description |
| `repository` | - | Git URL (required) |
| `wildcard` | `packages.x86_64-linux.*` | Attr-path pattern picked up by the evaluator. The legacy name `evaluation_wildcard` is still accepted as an alias |
| `active` | `true` | Disable to pause polling/evaluations without deleting |
| `keep_evaluations` | `30` | Number of finished (completed/failed) evaluations to retain per project. In-progress evaluations are never deleted and never count toward this limit; aborted evaluations are kept only to fill the limit when too few finished ones exist. Must be at least 1. Capped at runtime by the global `services.gradient.settings.keepEvaluations` |
| `concurrency` | `"skip"` | Policy for handling new trigger events while an evaluation is in flight (`hard_abort`, `soft_abort`, `skip`, `all`). Applies to all triggers on the project |
| `sign_cache` | `true` | When `false`, build outputs from this project are pushed to the cache but their narinfo signatures are left empty. External Nix clients won't trust them, keeping the project's outputs private even when the cache itself is public. A path co-produced by another `sign_cache=true` project is still signed |
| `outbound_integration` | `null` | Name of an `outbound` integration that receives CI status reports |
| `created_by` | - | Username of creator (required) |

`outbound_integration` must reference an entry in `services.gradient.state.integrations` belonging to the same organization. See [Integrations](#integrations) below.

To route inbound forge webhooks to a project, declare one or more `reporter_push` or `reporter_pull_request` triggers referencing the integration. See the [Triggers](#triggers) section below.

## Integrations

Forge integrations either receive push webhooks from the forge (`inbound`) or push CI status updates back to it (`outbound`). They are referenced from projects via `inbound_integration` / `outbound_integration`.

```nix
services.gradient.state.integrations = {
  acme-prod-inbound = {
    organization = "acme";
    kind         = "inbound";
    forge_type   = "gitea";          # gitea | forgejo | gitlab | github
    secret_file  = "/run/secrets/acme-inbound-hmac";
    created_by   = "alice";
  };

  acme-status-reports = {
    organization      = "acme";
    kind              = "outbound";
    forge_type        = "gitea";
    endpoint_url      = "https://gitea.example.com";
    access_token_file = "/run/secrets/acme-gitea-token";
    created_by        = "alice";
  };
};
```

### Integration options

| Option | Default | Description |
|---|---|---|
| `name` | `<attrset key>` | Unique within `(organization, kind)` |
| `display_name` | `null` (= `name`) | Human-readable label |
| `organization` | - | Owning organization (required) |
| `kind` | - | `inbound` (forge → Gradient) or `outbound` (Gradient → forge) |
| `forge_type` | - | `gitea`, `forgejo`, `gitlab`, or `github` |
| `secret_file` | `null` | HMAC secret for inbound webhooks. Encrypted into the DB at startup |
| `endpoint_url` | `null` | Base URL of the forge API. Outbound only |
| `access_token_file` | `null` | API token for outbound. Ignored for GitHub outbound (uses the GitHub App credentials) |
| `created_by` | - | Username of creator (required) |

A single inbound integration row serves Gitea, Forgejo and GitLab simultaneously - the actual forge is selected by the `/hooks/{forge}/{org}` URL path. The `forge_type` field is display metadata for inbound entries.

## Caches

```nix
services.gradient.state.caches = {
  main = {
    display_name     = "Main";
    description      = "Production binary cache";
    priority         = 10;
    local_priority   = 1;    # served to clients in services.gradient.settings.localIps
    max_storage_gb   = 0;    # 0 = unlimited
    public           = false;
    signing_key_file = "/run/secrets/cache-signing-key";
    organizations    = [ "acme" ];
    upstreams = [
      {
        type        = "external";
        display_name = "cache.nixos.org";
        url         = "https://cache.nixos.org";
        public_key  = "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=";
      }
      {
        type       = "internal";
        cache_name = "shared";
        mode       = "ReadOnly";
      }
    ];
    created_by = "alice";
  };
};
```

Generate a Nix cache signing key with:

```sh
nix-store --generate-binary-cache-key main-cache \
  /run/secrets/cache-signing-key \
  /run/secrets/cache-signing-key.pub
```

`nix-store` writes the keys with a `<name>:` prefix (e.g.
`main-cache:AbCd…`). The state provisioner expects the **raw base64
payload only** - strip the `main-cache:` prefix before wiring the file
into `signing_key_file`:

```sh
sed -i 's/^[^:]*://' /run/secrets/cache-signing-key
sed -i 's/^[^:]*://' /run/secrets/cache-signing-key.pub
```

Without this, startup fails with
`Signing key for cache '…' is not a valid base64 encoded string`.

### Cache options

| Option | Default | Description |
|---|---|---|
| `name` | `<attrset key>` | Unique cache name |
| `display_name` | `<name>` | Display name |
| `description` | `null` | Optional description |
| `active` | `true` | Set false to disable serving without deleting |
| `priority` | `10` | Higher wins when multiple caches contain the same path |
| `local_priority` | `null` | Alternate priority returned in `nix-cache-info` for clients whose IP matches `services.gradient.settings.localIps`. Null or 0 disables the override. |
| `max_storage_gb` | `0` | Max storage for this cache in GB. When all writable caches for an org have less than 10 MiB headroom, new evaluations park in `Waiting`. 0 = unlimited. |
| `signing_key_file` | - | Path to the (de-prefixed) base64 Ed25519 signing key (required) |
| `organizations` | `[]` | Organization names allowed to use this cache |
| `public` | `false` | Available to every organization |
| `upstreams` | `[ cache.nixos.org ]` | Substituters consulted on cache miss. See below |
| `created_by` | - | Username of creator (required) |

When `local_priority` is set to a non-null, non-zero integer, clients whose resolved IP falls within the `services.gradient.settings.localIps` CIDR list receive that value as the `Priority` field in the `nix-cache-info` response instead of the regular `priority`. This allows LAN clients to prefer a local cache over remote substituters without altering the priority seen by external clients. Null or 0 disables the override entirely.

### Upstream options

Each entry in `upstreams` is one of:

| Option | Type | Description |
|---|---|---|
| `type` | `"internal"` \| `"external"` | Whether the upstream is another Gradient cache or a plain Nix binary cache URL |
| `cache_name` | string | (`internal`) Name of the Gradient cache to subscribe to |
| `display_name` | string \| null | Optional label. Defaults to `cache_name` for `internal`, required for `external` |
| `mode` | `"ReadWrite"` \| `"ReadOnly"` \| `"WriteOnly"` | (`internal` only) Subscription mode. Defaults to `ReadWrite`. Ignored for `external`, which is always `ReadOnly` |
| `url` | string | (`external`) Substituter URL - e.g. `https://cache.nixos.org` |
| `public_key` | string | (`external`) Trusted public key in `<name>:<base64>` form |

Outbound requests to `external` substituters (and to every other HTTP target the
server, worker, or CLI talks to) carry the user-agent
`Gradient/<version> (+https://github.com/wavelens/gradient)`, so cache operators
can attribute traffic and build allowlists or per-client metrics around it.

## Roles

State files can declare custom org-scoped roles via the `roles` attribute.
Each role targets one organization and grants a fixed permission set:

```nix
services.gradient.state.roles = {
  releaser = {
    organization = "acme";
    permissions  = [ "viewOrg" "triggerEvaluation" ];
  };
};
```

Managed roles are immutable through the role-management API: `PATCH` and
`DELETE` return `403 Forbidden`. Removing the entry from the state file
unmarks the role (or deletes it, when `settings.deleteState = true`).

Role names must not collide with the built-in roles (`Admin`, `Write`,
`View`) or with another state-managed role in the same organization -
startup fails on collision.

### Mapping OIDC groups to roles

A role may list `oidc_group` values. On each OIDC login, a user whose
`groups` claim contains any listed group is granted that role in the role's
organization (creating the membership if missing, upgrading the role if it
differs). Grants are **additive**: OIDC groups only ever add or upgrade a
membership, never remove one - removal stays with explicit `members` lists
and the API. This targets state-managed custom roles only; to grant
admin-level access via a group, declare a custom role with those permissions
and an `oidc_group`. Requires the `groups` scope on the OIDC client (add
`"groups"` to `services.gradient.oidc.scopes`).

```nix
services.gradient.state.roles.platform-admin = {
  organization = "acme";
  permissions  = [ "viewOrg" "triggerEvaluation" ];
  oidc_group   = [ "platform-team" "ops" ];
};
```

### Role options

| Option | Default | Description |
|---|---|---|
| `name` | `<attrset key>` | Role name. Must be unique within the organization and must not collide with a built-in role |
| `organization` | - | Owning organization name (required) |
| `permissions` | - | List of capability identifiers granted by the role (required, see `GET /user/keys/permissions` for the catalogue) |
| `oidc_group` | `[]` | OIDC group claims that grant this role on login (additive). Requires the `groups` scope |

## API Keys

```nix
services.gradient.state.api_keys = {
  ci-runner = {
    key_file     = "/run/secrets/ci-api-key";
    owned_by     = "alice";
    permissions  = [ "viewOrg" "triggerEvaluation" ];
    organization = "acme";        # optional - omit for an unscoped key
  };
};
```

The key file must contain the **lowercase 64-char SHA-256 hex digest** of the
token (without the `GRAD` prefix). The server stores keys hashed; only the hash
ends up in the database. Generate one for an existing token (or a new random
one) like so:

```sh
TOKEN="$(openssl rand -hex 32)"
printf %s "$TOKEN" | sha256sum | cut -d' ' -f1 > /run/secrets/ci-api-key
```

Hand `GRAD$TOKEN` to the user/CI pipeline; the server will hash it on the way
in and compare against the digest in `key_file`.

The `permissions` list is **required** - there is no safe default. When
`organization` is set, the key is rejected for every other org (404, so org
existence isn't leaked).

### API-key options

| Option | Default | Description |
|---|---|---|
| `name` | `<attrset key>` | Unique key name |
| `key_file` | - | Path to a file containing the lowercase 64-char SHA-256 hex digest of the token (required) |
| `owned_by` | - | Username that owns the key (required) |
| `permissions` | - | Capability identifiers the key grants (required, non-empty). See `GET /user/keys/permissions` for the catalogue |
| `organization` | `null` | Organization name to pin the key to. Omit for an unscoped key |

## Workers

Worker registrations can be declared in state instead of using the API. This is useful for NixOS-managed build machines where you want tokens to be provisioned automatically.

```nix
services.gradient.state.workers = {
  builder-1 = {
    display_name  = "Primary Build Server";  # defaults to attrset key
    worker_id     = "550e8400-e29b-41d4-a716-446655440001";
    organizations = [ "acme" ];               # one row per (worker_id, org)
    token_file    = "/run/secrets/builder-1-token";
    created_by    = "alice";

    # Optional: have the server dial the worker instead of waiting for it.
    # url = "wss://builder-1.example.com/proto";

    # Per-registration capability gates - clear one to refuse the capability
    # for this worker even if the worker advertises it.
    enable_fetch = true;
    enable_eval  = true;
    enable_build = true;
  };
};
```

The token file must contain a single plaintext token - the server hashes it and stores the result, the plaintext is never persisted:

```sh
openssl rand -base64 48 > /run/secrets/builder-1-token
```

`worker_id` is required and must match the `GRADIENT_WORKER_ID` environment variable (or `workerId` option) on the worker machine. Unlike API registration, state-managed workers are not restricted to UUID v4 - any stable string is accepted, though using a UUID is conventional.

To ensure the worker uses the same ID that was pre-registered, set `workerId` in the worker module:

```nix
services.gradient.worker.workerId = "550e8400-e29b-41d4-a716-446655440001";
```

State-managed worker registrations are deleted automatically when removed from `state.workers`, and per-(worker_id, organization) rows are deleted when an organization is dropped from `organizations` (subject to `settings.deleteState`).

On the worker machine, the `peersFile` authenticates with `<org_id>:<token>` lines. The `org_id` is the organization's UUID — to know it ahead of the first server start, pin it with `state.organizations.<name>.id` and reference that same value in the worker's `peersFile`. The `*:<token>` wildcard remains the alternative when a single token may serve any org.

### Worker options

| Option | Default | Description |
|---|---|---|
| `display_name` | `<attrset key>` | Display name shown in the workers list |
| `worker_id` | - | Persistent worker identity. Must match the worker's `GRADIENT_WORKER_ID` (required) |
| `organizations` | - | List of organizations the worker is registered under. One row per (worker_id, organization). Must contain at least one entry (required) |
| `token_file` | - | Plaintext token file. Hashed at provision time (required) |
| `url` | `null` | When set, the server dials the worker at this WebSocket URL instead of waiting for an inbound connection |
| `enable_fetch` | `true` | Server-side gate for the `fetch` capability |
| `enable_eval` | `true` | Server-side gate for the `eval` capability |
| `enable_build` | `true` | Server-side gate for the `build` capability |
| `created_by` | - | Username of creator (required) |

## Triggers

Each project can have one or more triggers that decide *when* an evaluation runs.
Triggers are configurable via the API or declaratively in state files.

The concurrency policy - what happens when a new trigger event arrives while an evaluation is already in flight - is a **project-level** setting, not per-trigger. Set it on the project with `concurrency` (see [Project options](#project-options) above).

```nix
services.gradient.state.projects.my-project = {
  # ... other project options ...
  concurrency = "hard_abort";   # applies to all triggers below
  triggers = [
    {
      type = "polling";
      config = { interval_secs = 60; branch = "main"; };
    }
    {
      type = "reporter_push";
      integration = "gitea-prod";          # name of an inbound integration in the same org
      config = {
        branches = [ "main" "release/*" ];
        tags = [];
        releases_only = false;
      };
    }
    {
      type = "reporter_pull_request";
      integration = "gitea-prod";
      config = {
        branches = [ "main" ];
        actions = [ "opened" "synchronize" "reopened" ];
      };
    }
    {
      type = "time";
      config = { cron = "0 0 2 * * *"; };   # 02:00 UTC every day (six-field: sec min hour dom mon dow)
    }
  ];
};
```

### Trigger types

- **polling** - periodically check the git repository for new commits. `interval_secs` minimum 10, default 300. Each cycle is jittered by up to 10% of `interval_secs` (deterministic per trigger and cycle) so that triggers created together don't pile onto the same upstream tick. **branch** (optional) - track a specific branch; leave unset to follow the remote HEAD (the repo's default branch).
- **reporter_push** - fires on forge push events. Filters: `branches`, `tags` (glob patterns; empty = match all), `releases_only` (only fires on explicit forge release events).
- **reporter_pull_request** - fires on PR/MR events. Filters: `branches`, `actions` (default: opened/synchronize/reopened).
- **time** - fires on a six-field cron schedule (UTC). Re-evaluates the project HEAD even if the commit hasn't changed.

`reporter_push` and `reporter_pull_request` triggers must reference an **`inbound`** integration - the row whose `secret_file` validates incoming forge webhooks. Pointing one at an `outbound` integration is rejected at startup; outbound integrations are wired up separately via the project's `outbound_integration` or a `forge_status_report` action. For non-GitHub forges this usually means declaring two integration rows (one `inbound`, one `outbound`).

### Concurrency policies

Each project has a single concurrency policy that applies to all of its triggers:

- **hard_abort** - cancel the in-flight evaluation and its in-flight builds, then start a new evaluation. Workers running affected builds receive cancellation through the existing job lifecycle.
- **soft_abort** - mark the in-flight evaluation `Aborted` so the new one becomes canonical, but let already-running builds finish; their cached outputs flow into the new evaluation.
- **skip** - discard the new trigger event; keep the running evaluation.
- **all** - run a new evaluation alongside the in-flight one. The new eval is flagged `concurrent` so it bypasses the "one active eval per project" guard while leaving that guard intact for `hard_abort` / `soft_abort` / `skip`.

### Defaults

- New projects automatically get a default `polling` trigger (interval 300s). Existing projects were backfilled by the same logic during the migration.
- Concurrency defaults to `soft_abort` - a new trigger event marks the running evaluation Aborted while letting its in-flight builds finish; the new evaluation reuses any cached outputs they produce. Switch to `hard_abort` to also cancel the running builds, `skip` to drop the new event, or `all` to run multiple evaluations concurrently.

The implicit fallback poll for projects with an inbound integration (the legacy `WEBHOOK_BACKUP_POLL_SECS` behavior) has been removed; webhook-driven projects must declare an explicit `reporter_push` trigger to receive evaluations from forge pushes.

## Exporting current state

When the live system has drifted from your Nix config - users registered through the UI, organizations or projects created via the API - `GET /admin/state` reconstructs the current users, organizations, projects, caches, custom roles, API keys, workers and integrations into the same shape as `services.gradient.state`, so you can codify the running system back into Nix.

The endpoint requires `superuser` and supports two formats:

```bash
# Nix expression (default), ready to paste under services.gradient.state
curl -H "Authorization: Bearer $TOKEN" https://gradient.example.com/api/v1/admin/state

# JSON, mirroring the StateConfiguration object
curl -H "Authorization: Bearer $TOKEN" "https://gradient.example.com/api/v1/admin/state?format=json"
```

Secrets are never recoverable from the database (passwords and worker tokens are hashed, signing keys and integration secrets are encrypted). Every credential-file field - `password_file`, `private_key_file`, `signing_key_file`, `token_file`, `key_file`, `secret_file`, `access_token_file` - is therefore exported as `null`; fill in the credential paths on your host before applying. The auto-managed `build-request` project, server-managed GitHub integration rows, and the built-in `Admin`/`Write`/`View` roles are omitted.
