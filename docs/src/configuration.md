# Configuration

Gradient is configured exclusively through its **NixOS module**. There is no configuration file or command-line flags — all options are set in your NixOS configuration under `services.gradient`.

## Minimal Setup

```nix
services.gradient = {
  enable            = true;
  frontend.enable   = true;
  configurePostgres = true;
  configureNginx    = true;
  domain            = "gradient.example.com";
  jwtSecretFile     = "/run/secrets/gradient-jwt";
  cryptSecretFile   = "/run/secrets/gradient-crypt";
};
```

`configurePostgres` creates a local PostgreSQL database and user. `configureNginx` adds a virtual host that proxies `/api/`, `/proto`, and `/cache/` to the backend and serves the frontend SPA.

## Secrets

Two secrets are required. Generate them with:

```sh
# JWT signing key (HS256, minimum 32 bytes)
openssl rand -base64 48 > /run/secrets/gradient-jwt

# Database encryption key
openssl rand -base64 48 > /run/secrets/gradient-crypt
```

!!! warning
    Never commit secret files to version control. Use [sops-nix](https://github.com/Mic92/sops-nix) or [agenix](https://github.com/ryantm/agenix) to manage them.

## All Options

| Option | Default | Description |
|---|---|---|
| `domain` | — | Public hostname (required) |
| `baseDir` | `/var/lib/gradient` | Data directory |
| `listenAddr` | `127.0.0.1` | Bind address |
| `port` | `3000` | HTTP port |
| `jwtSecretFile` | — | Path to JWT secret file (required) |
| `cryptSecretFile` | — | Path to encryption secret file (required) |
| `databaseUrlFile` | auto | Override the PostgreSQL connection string file |
| `serveCache` | `false` | Enable Nix binary cache serving |
| `reportErrors` | `false` | Send errors to Sentry |
| `discoverable` | `true` | Accept incoming `/proto` WebSocket connections from workers |
| `settings.maxProtoConnections` | `256` | Max simultaneous worker WebSocket connections |
| `settings.keepEvaluations` | `5` | Number of evaluations kept per project |
| `settings.logLevel.default` | `info` | Log level: `trace` `debug` `info` `warn` `error` |
| `settings.logLevel.cache` | null | Cache log level override (null inherits default) |
| `settings.logLevel.web` | null | Web log level override (null inherits default) |
| `settings.logLevel.proto` | null | Proto log level override (null inherits default) |
| `settings.enableRegistration` | `true` | Allow new user self-registration |
| `settings.deleteState` | `true` | Remove entities no longer in `state` (see below) |
| `settings.cacheTtlHours` | `336` | TTL in hours for cached NARs not fetched recently (0 = disabled) |

## OIDC

```nix
services.gradient.oidc = {
  enable           = true;
  required         = false;   # set true to disable username/password login and require OIDC for all users
  clientId         = "gradient";
  clientSecretFile = "/run/secrets/gradient-oidc-secret";
  discoveryUrl     = "https://auth.example.com";
  scopes           = [ "openid" "email" "profile" ];
  iconUrl          = null;    # optional provider logo URL
};
```

Gradient uses PKCE and discovers all provider endpoints from `discoveryUrl/.well-known/openid-configuration` and callback url is at `https://$domain/api/v1/auth/oidc/callback`. Set `required` to `true` to disable basic auth and require OIDC for all users.

## Email

```nix
services.gradient.email = {
  enable              = true;
  requireVerification = true;
  smtpHost            = "smtp.example.com";
  smtpPort            = 587;
  smtpUsername        = "gradient@example.com";
  smtpPasswordFile    = "/run/secrets/gradient-smtp";
  fromAddress         = "gradient@example.com";
  fromName            = "Gradient";
};
```

## GitHub App

A GitHub App provides automatic webhook delivery and CI status reporting without per-project tokens. One App covers all organizations on the instance.

### Setup

1. Create a GitHub App at `github.com → Settings → Developer settings → GitHub Apps → New GitHub App`.
   - **Webhook URL**: `https://gradient.example.com/api/v1/hooks/github`
   - **Webhook secret**: generate a random value and note it
   - **Permissions**: Repository → Commit statuses (Read & Write), Repository → Contents (Read)
   - **Subscribe to events**: Push, Installation

2. After creation note the **App ID** and download the **private key** PEM.

3. Configure Gradient:

```nix
services.gradient.githubApp = {
  enable             = true;
  appId              = 123456;
  privateKeyFile     = "/run/secrets/gradient-github-app-key";
  webhookSecretFile  = "/run/secrets/gradient-github-app-webhook-secret";
};
```

4. Install the App on each GitHub organization. Gradient auto-stores the `installation_id` from the webhook.

5. Once installed, push events automatically trigger evaluations (no polling) and CI statuses are reported using the installation token instead of a per-project PAT.

## Forge Webhooks (Gitea / Forgejo / GitLab / GitHub without App)

For non-GitHub forges or GitHub without the App, configure a per-organization webhook secret via the UI:

1. Open **Organization → Settings → Forge Webhooks** and click **Generate Webhook Secret**.
2. Copy the displayed **Webhook URL** and **Secret**.
3. In your forge, create a push webhook pointing to the URL, using the secret for HMAC-SHA256 signing.

Forge path by type:

| Forge | URL path segment | Signature header |
|---|---|---|
| Gitea / Forgejo | `/hooks/gitea/{org}` or `/hooks/forgejo/{org}` | `X-Gitea-Signature` |
| GitLab | `/hooks/gitlab/{org}` | `X-Gitlab-Token` |
| GitHub (no App) | `/hooks/github/{org}` | `X-Hub-Signature-256` |

Gradient matches the incoming push payload's clone URL against active projects and queues an evaluation immediately.

## Workers

Build capacity is provided by **`gradient-worker`** instances that connect to the server over a WebSocket at `/proto`. Each worker is a separate process and can run on the same host or a remote machine.

### Local Worker (default)

Every Gradient server automatically starts a co-located worker:

```nix
services.gradient.workers.local = {
  enable    = true;
  serverUrl = "ws://127.0.0.1:3000/proto";
  capabilities = {
    fetch = true;
    eval  = true;
    build = true;
    sign  = true;
  };
};
```

This is enabled by default (`lib.mkDefault true`). Override any value to tune it.

### Remote Workers

Deploy `gradient-worker` on additional machines by importing the worker module:

```nix
# On the remote build machine:
imports = [ inputs.gradient.nixosModules.default ];

services.gradient.workers.main = {
  enable    = true;
  serverUrl = "wss://gradient.example.com/proto";

  # Authenticate against an organization that registered this worker
  # (see POST /api/v1/orgs/{org}/workers for registration)
  peersFile = "/run/secrets/gradient-worker-peers";

  capabilities = {
    fetch = true;
    eval  = true;
    build = true;
    sign  = true;
  };

  settings = {
    maxConcurrentBuilds       = 8;
    evalWorkers               = 2;
    maxConcurrentEvaluations  = 2;
  };
};
```

The `peersFile` must contain `peer_id:token` pairs (comma-separated). Obtain the `peer_id` and `token` by registering the worker under an organization:

```sh
curl -X POST https://gradient.example.com/api/v1/orgs/myorg/workers \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"worker_id": "my-builder-hostname"}'
# → {"error":false,"message":{"peer_id":"<uuid>","token":"<token>"}}
```

Write the result to the peers file:

```sh
echo "<peer_id>:<token>" > /run/secrets/gradient-worker-peers
```

When `peersFile` is `null` (the default), the worker connects in **open mode** — suitable for co-located workers that you trust implicitly. The server accepts the connection if no peers have been explicitly registered for that worker ID.

### Worker Options

| Option | Default | Description |
|---|---|---|
| `serverUrl` | — | WebSocket URL of the server's `/proto` endpoint (required) |
| `peersFile` | `null` | Path to `peer_id:token,...` auth file; null = open mode |
| `discoverable` | `false` | Accept incoming connections from the server (reverse-proxy mode) |
| `port` | `3100` | Listener port when `discoverable` is enabled |
| `capabilities.fetch` | `false` | Prefetch flake inputs |
| `capabilities.eval` | `false` | Run Nix evaluations |
| `capabilities.build` | `false` | Execute Nix builds |
| `capabilities.sign` | `false` | Sign store paths |
| `capabilities.federate` | `false` | Act as a federation relay (requires `discoverable`) |
| `settings.maxConcurrentBuilds` | `100` | Parallel build slots |
| `settings.evalWorkers` | `1` | Number of evaluator subprocesses |
| `settings.maxConcurrentEvaluations` | `1` | Parallel evaluations |
| `settings.maxEvaluationsPerWorker` | `20` | Recycle evaluator subprocess after N jobs (0 = never) |
| `settings.evalClosureParallelism` | `8` | Parallel closure walkers during evaluation |
| `settings.maxNixdaemonConnections` | `24` | Nix daemon connection pool size |
| `settings.maxProtoConnections` | `16` | Max simultaneous WebSocket connections (for discoverable mode) |
| `settings.logLevel.default` | `info` | Worker log level |
| `settings.logLevel.eval` | null | Evaluator log level override |
| `settings.logLevel.build` | null | Builder log level override |
| `settings.logLevel.proto` | null | Protocol log level override |

## Declarative State

`services.gradient.state` lets you declare users, organizations, projects, caches, and API keys in Nix. Gradient reconciles this state on every startup.

When `settings.deleteState = true` (default), entities that are removed from `state` are also deleted from the database. Set it to `false` to make them editable by users in the frontend instead.

### Users

```nix
services.gradient.state.users = {
  alice = {
    name           = "Alice";
    email          = "alice@example.com";
    password_file  = "/run/secrets/alice-password";
    email_verified = true;
    superuser      = false;
  };
};
```

The password file must contain an **argon2id PHC hash**. Generate one with:

```sh
nix shell nixpkgs#libargon2 -c \
  sh -c 'argon2 "$(openssl rand -hex 16)" -id -e <<< "mypassword"' \
  > /run/secrets/alice-password
```

### Organizations

```nix
services.gradient.state.organizations = {
  acme = {
    display_name     = "ACME Corp";
    private_key_file = "/run/secrets/acme-ssh-key";
    created_by       = "alice";
  };
};
```

The SSH private key is the organization's identity key used to clone Git repositories over SSH. Generate one with:

```sh
ssh-keygen -t ed25519 -N "" -f /run/secrets/acme-ssh-key
# Add the public key (.pub) to your Git host as a deploy key
```

### Projects

```nix
services.gradient.state.projects = {
  web-app = {
    organization        = "acme";
    repository          = "git@github.com:acme/web-app.git";
    evaluation_wildcard = "packages.x86_64-linux.*";
    created_by          = "alice";
  };
};
```

### Caches

```nix
services.gradient.state.caches = {
  main = {
    signing_key_file = "/run/secrets/cache-signing-key";
    organizations    = [ "acme" ];
    created_by       = "alice";
  };
};
```

Generate a Nix cache signing key with:

```sh
nix-store --generate-binary-cache-key main-cache \
  /run/secrets/cache-signing-key \
  /run/secrets/cache-signing-key.pub
```

### API Keys

```nix
services.gradient.state.api_keys = {
  ci-token = {
    key_file = "/run/secrets/ci-api-key";
    owned_by = "alice";
  };
};
```

The key file must contain a token with the `GRAD` prefix:

```sh
echo "GRAD$(openssl rand -hex 32)" > /run/secrets/ci-api-key
```
