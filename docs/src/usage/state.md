# Declarative State

`services.gradient.state` lets you declare users, organizations, projects, caches, and API keys in Nix. Gradient reconciles this state on every startup.

When `settings.deleteState = true` (default), entities that are removed from `state` are also deleted from the database. Set it to `false` to make them editable by users in the frontend instead.

## State-Managed Resources

Users, organizations, and caches created by the NixOS module configuration carry `managed = true`. The API rejects mutations and deletions of these records with `403 Forbidden`. This allows declarative configuration to be the source of truth without Gradient's UI overwriting it.

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

> **Do not** feed the password via a bash herestring (`<<< "mypassword"`) —
> herestrings append a trailing newline, so `argon2` would hash
> `mypassword\n` and later logins with `mypassword` would fail. Use
> `printf %s` (no `\n`) or `gradient hash`.

At server startup, the file content is validated to start with `$argon2`
and stored verbatim — the server never sees the plaintext password.

Set `password_file = null` (or omit it) for users that authenticate
exclusively via OIDC. Provisioning a user *with* a password and then
attempting to sign in as that user via OIDC is rejected by the server
(`User already exists with password authentication`), so OIDC-only users
must be declared without one.

### User options

| Option | Default | Description |
|---|---|---|
| `username` | `<attrset key>` | Unique username |
| `name` | `<username>` | Display name |
| `email` | — | Email address (required) |
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

### Organization options

| Option | Default | Description |
|---|---|---|
| `name` | `<attrset key>` | Unique organization name |
| `display_name` | `<name>` | Display name |
| `description` | `null` | Optional description |
| `private_key_file` | — | Path to SSH private key (required) |
| `public` | `false` | Visible to all users |
| `github_installation_id` | `null` | GitHub App installation id to bind to this org (look it up on the App's "Install App" page on GitHub). Setting this enables outbound CI status reporting and webhook routing. When `null`, the field is left untouched on update so a webhook-recorded id survives reconciliation |
| `created_by` | — | Username of creator (required) |

## Projects

```nix
services.gradient.state.projects = {
  web-app = {
    organization         = "acme";
    display_name         = "Web App";
    description          = "Production web application";
    repository           = "git@github.com:acme/web-app.git";
    evaluation_wildcard  = "packages.x86_64-linux.*";
    active               = true;
    inbound_integration  = "acme-prod-inbound";   # optional
    outbound_integration = "acme-status-reports"; # optional
    created_by           = "alice";
  };
};
```

### Project options

| Option | Default | Description |
|---|---|---|
| `name` | `<attrset key>` | Unique project name |
| `organization` | — | Owning organization (required) |
| `display_name` | `<name>` | Display name |
| `description` | `null` | Optional description |
| `repository` | — | Git URL (required) |
| `evaluation_wildcard` | `packages.x86_64-linux.*` | Attr-path pattern picked up by the evaluator |
| `active` | `true` | Disable to pause polling/evaluations without deleting |
| `force_evaluation` | `false` | Re-evaluate on next poll regardless of the last commit hash |
| `inbound_integration` | `null` | Name of an `inbound` integration (same org) routing webhooks to this project |
| `outbound_integration` | `null` | Name of an `outbound` integration that receives CI status reports |
| `created_by` | — | Username of creator (required) |

`inbound_integration` / `outbound_integration` must reference an entry in `services.gradient.state.integrations` belonging to the same organization. See [Integrations](#integrations) below.

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
| `organization` | — | Owning organization (required) |
| `kind` | — | `inbound` (forge → Gradient) or `outbound` (Gradient → forge) |
| `forge_type` | — | `gitea`, `forgejo`, `gitlab`, or `github` |
| `secret_file` | `null` | HMAC secret for inbound webhooks. Encrypted into the DB at startup |
| `endpoint_url` | `null` | Base URL of the forge API. Outbound only |
| `access_token_file` | `null` | API token for outbound. Ignored for GitHub outbound (uses the GitHub App credentials) |
| `created_by` | — | Username of creator (required) |

A single inbound integration row serves Gitea, Forgejo and GitLab simultaneously — the actual forge is selected by the `/hooks/{forge}/{org}` URL path. The `forge_type` field is display metadata for inbound entries.

## Caches

```nix
services.gradient.state.caches = {
  main = {
    display_name     = "Main";
    description      = "Production binary cache";
    priority         = 10;
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
payload only** — strip the `main-cache:` prefix before wiring the file
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
| `signing_key_file` | — | Path to the (de-prefixed) base64 Ed25519 signing key (required) |
| `organizations` | `[]` | Organization names allowed to use this cache |
| `public` | `false` | Available to every organization |
| `upstreams` | `[ cache.nixos.org ]` | Substituters consulted on cache miss. See below |
| `created_by` | — | Username of creator (required) |

### Upstream options

Each entry in `upstreams` is one of:

| Option | Type | Description |
|---|---|---|
| `type` | `"internal"` \| `"external"` | Whether the upstream is another Gradient cache or a plain Nix binary cache URL |
| `cache_name` | string | (`internal`) Name of the Gradient cache to subscribe to |
| `display_name` | string \| null | Optional label. Defaults to `cache_name` for `internal`, required for `external` |
| `mode` | `"ReadWrite"` \| `"ReadOnly"` \| `"WriteOnly"` | (`internal` only) Subscription mode. Defaults to `ReadWrite`. Ignored for `external`, which is always `ReadOnly` |
| `url` | string | (`external`) Substituter URL — e.g. `https://cache.nixos.org` |
| `public_key` | string | (`external`) Trusted public key in `<name>:<base64>` form |

## API Keys

```nix
services.gradient.state.api_keys = {
  ci-token = {
    key_file = "/run/secrets/ci-api-key";
    owned_by = "alice";
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

### API-key options

| Option | Default | Description |
|---|---|---|
| `name` | `<attrset key>` | Unique key name |
| `key_file` | — | Path to a file containing the lowercase 64-char SHA-256 hex digest of the token (required) |
| `owned_by` | — | Username that owns the key (required) |

## Workers

Worker registrations can be declared in state instead of using the API. This is useful for NixOS-managed build machines where you want tokens to be provisioned automatically.

```nix
services.gradient.state.workers = {
  builder-1 = {
    display_name = "Primary Build Server";  # defaults to attrset key
    worker_id    = "550e8400-e29b-41d4-a716-446655440001";
    organization = "acme";
    token_file   = "/run/secrets/builder-1-token";
    created_by   = "alice";

    # Optional: have the server dial the worker instead of waiting for it.
    # url = "wss://builder-1.example.com/proto";

    # Per-registration capability gates — clear one to refuse the capability
    # for this worker even if the worker advertises it.
    enable_fetch = true;
    enable_eval  = true;
    enable_build = true;
  };
};
```

The token file must contain a single plaintext token — the server hashes it and stores the result, the plaintext is never persisted:

```sh
openssl rand -base64 48 > /run/secrets/builder-1-token
```

`worker_id` is required and must match the `GRADIENT_WORKER_ID` environment variable (or `workerId` option) on the worker machine. Unlike API registration, state-managed workers are not restricted to UUID v4 — any stable string is accepted, though using a UUID is conventional.

To ensure the worker uses the same ID that was pre-registered, set `workerId` in the worker module:

```nix
services.gradient.worker.workerId = "550e8400-e29b-41d4-a716-446655440001";
```

State-managed worker registrations are deleted automatically when removed from `state.workers` (subject to `settings.deleteState`).

### Worker options

| Option | Default | Description |
|---|---|---|
| `display_name` | `<attrset key>` | Display name shown in the workers list |
| `worker_id` | — | Persistent worker identity. Must match the worker's `GRADIENT_WORKER_ID` (required) |
| `organization` | — | Owning organization (required) |
| `token_file` | — | Plaintext token file. Hashed at provision time (required) |
| `url` | `null` | When set, the server dials the worker at this WebSocket URL instead of waiting for an inbound connection |
| `enable_fetch` | `true` | Server-side gate for the `fetch` capability |
| `enable_eval` | `true` | Server-side gate for the `eval` capability |
| `enable_build` | `true` | Server-side gate for the `build` capability |
| `created_by` | — | Username of creator (required) |

## Triggers

Each project can have one or more triggers that decide *when* an evaluation runs.
Triggers are configurable via the API or declaratively in state files.

```nix
services.gradient.state.projects.my-project.triggers = [
  {
    type = "polling";
    config = { interval_secs = 60; };
    concurrency = "skip";
  }
  {
    type = "reporter_push";
    integration = "gitea-prod";          # name of an inbound integration in the same org
    config = {
      branches = [ "main" "release/*" ];
      tags = [];
      releases_only = false;
    };
    concurrency = "hard_abort";
  }
  {
    type = "reporter_pull_request";
    integration = "gitea-prod";
    config = {
      branches = [ "main" ];
      actions = [ "opened" "synchronize" "reopened" ];
    };
    concurrency = "hard_abort";
  }
  {
    type = "time";
    config = { cron = "0 0 2 * * *"; };   # 02:00 UTC every day (six-field: sec min hour dom mon dow)
    concurrency = "skip";
  }
];
```

### Trigger types

- **polling** — periodically check the git repository for new commits. `interval_secs` minimum 10, default 300.
- **reporter_push** — fires on forge push events. Filters: `branches`, `tags` (glob patterns; empty = match all), `releases_only` (only fires on explicit forge release events).
- **reporter_pull_request** — fires on PR/MR events. Filters: `branches`, `actions` (default: opened/synchronize/reopened).
- **time** — fires on a six-field cron schedule (UTC). Re-evaluates the project HEAD even if the commit hasn't changed.

### Concurrency policies

- **hard_abort** — cancel the in-flight evaluation and its in-flight builds, then start a new evaluation. Workers running affected builds receive cancellation through the existing job lifecycle.
- **soft_abort** — mark the in-flight evaluation `Aborted` so the new one becomes canonical, but let already-running builds finish; their cached outputs flow into the new evaluation.
- **allow** — *reserved.* Currently rejected with HTTP 400; multi-evaluation-per-project support is a follow-up.
- **skip** — discard the new trigger event; keep the running evaluation.

### Defaults

- New projects automatically get a default `polling` trigger (interval 300s, concurrency `skip`). Existing projects were backfilled by the same logic during the migration.
- Reporter push/PR triggers default to `hard_abort` (typical CI semantics).
- Polling and time triggers default to `skip` in user-created configurations.

The implicit fallback poll for projects with an inbound integration (the legacy `WEBHOOK_BACKUP_POLL_SECS` behavior) has been removed; webhook-driven projects must declare an explicit `reporter_push` trigger to receive evaluations from forge pushes.
