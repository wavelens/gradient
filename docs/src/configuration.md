# Configuration

Gradient is configured exclusively through its **NixOS module**. There is no configuration file or command-line flags — all options are set in your NixOS configuration under `services.gradient`.

## Minimal Setup

```nix
services.gradient = {
  enable            = true;
  frontend.enable   = true;
  configurePostgres = true;
  reverseProxy.nginx.enable = true;
  domain            = "gradient.example.com";
  jwtSecretFile     = "/run/secrets/gradient-jwt";
  cryptSecretFile   = "/run/secrets/gradient-crypt";
};
```

`configurePostgres` creates a local PostgreSQL database and user. `reverseProxy` adds a virtual host that proxies `/api/`, `/proto`, and `/cache/` to the backend and serves the frontend SPA (either with `nginx` or `caddy` as a reverse proxy)

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

## Reverse Proxies

The Gradient server does not come with a built-in http server for the frontend. 
Therefore a reverse proxy / webserver is needed for hosting.
The nixos module provides two preconfigured reverse proxies:
- `nginx`
- `caddy`

### Nginx

| Option | Default | Description |
|--------|---------|-------------|
| `reverseProxy.nginx.enable` | `false` | Whether to enable nginx as the reverse proxy |

### Caddy

!!! note
    To match the upstream `services.caddy` configuration you have to manage the ACME host certificate yourself.

| Option | Default | Description |
|--------|---------|-------------|
| `reverseProxy.caddy.enable` | `false` | Whether to enable caddy as the reverse proxy |
| `reverseProxy.caddy.useACMEHost` | `null` | Passed directly to [`services.caddy.virtualHosts.<name>.useACMEHost`](https://search.nixos.org/options?channel=unstable&query=services.caddy.virtualHosts.&show=option:services.caddy.virtualHosts.%3Cname%3E.useACMEHost) |
| `reversePorxy.caddy.extraConfig` | `""` | Caddy config options written to [`services.caddy.virtualHosts.<name>.extraConfig`](https://search.nixos.org/options?channel=unstable&query=services.caddy.virtualHosts.&show=option:services.caddy.virtualHosts.%3Cname%3E.extraConfig) after the reverse proxy setup |

### Custom Reverse Proxy

If you want to use your own reverse proxy you have to setup redirects as follows:
- `https://example.com/api` _(with all subpaths)_ -> `http://${ADDR}:${PORT}/api`
- `https://example.com/proto` -> `http://${ADDR}:${PORT}/proto` _(must support websockets)_
- `https://example.com/cache` _(with all subpaths)_ -> `http://${ADDR}:${PORT}/cache`
All other requests should be handled by a static webserver hosting the files at:
- `${pkgs.gradient-frontend}/share/gradient-frontend`

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

Build capacity is provided by **`gradient-worker`** instances that connect to the server over a WebSocket at `/proto`. Workers are separate processes and can run on the same host or on dedicated build machines.

The server does **not** start a worker automatically. Configure one explicitly using the `gradient-worker` NixOS module.

### Co-located Worker

To run a worker on the same machine as the server, import the worker module and configure `services.gradient.worker`:

```nix
imports = [ inputs.gradient.nixosModules.gradient-worker ];

services.gradient.worker = {
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

### Remote Workers

Deploy `gradient-worker` on dedicated build machines. First register the worker under an organization — either declaratively via `state.workers` (see below) or via the API. The `worker_id` must be a **UUID v4**. The worker auto-generates one on first start and persists it to `/var/lib/gradient-worker/worker-id`:

```sh
cat /var/lib/gradient-worker/worker-id
```

```sh
curl -X POST https://gradient.example.com/api/v1/orgs/myorg/workers \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"worker_id": "550e8400-e29b-41d4-a716-446655440001"}'
# → {"error":false,"message":{"peer_id":"<uuid>","token":"<token>"}}
```

You can optionally pre-generate the token and pass it in the request (`openssl rand -base64 48`); the response will then omit the token field.

Then on the build machine:

```nix
imports = [ inputs.gradient.nixosModules.gradient-worker ];

services.gradient.worker = {
  enable    = true;
  serverUrl = "wss://gradient.example.com/proto";
  peersFile = "/run/secrets/gradient-worker-peers";

  capabilities = {
    fetch = true;
    eval  = true;
    build = true;
    sign  = true;
  };

  settings = {
    maxConcurrentBuilds      = 8;
    evalWorkers              = 2;
    maxConcurrentEvaluations = 2;
  };
};
```

Write the registration result to the peers file (one `peer_id:token` pair per line):

```sh
echo "<peer_id>:<token>" > /run/secrets/gradient-worker-peers
```

The special peer ID `*` can be used instead of a specific UUID to respond with that token for any peer the server challenges:

```
# /run/secrets/gradient-worker-peers
*:<token>
```

The token must be the 48-byte random secret returned by the registration API (generated via `openssl rand -base64 48` server-side).

### Worker Options

| Option | Default | Description |
|---|---|---|
| `serverUrl` | `null` | WebSocket URL of the server's `/proto` endpoint (required) |
| `workerId` | `null` | Override the worker UUID (`GRADIENT_WORKER_ID`). When null, the ID is read from `$StateDirectory/worker-id` or auto-generated on first start |
| `peersFile` | `null` | Path to peers file (`peer_id:token` per line, `*` = any peer) |
| `useTls` | `true` | Enable TLS (ACME + forceSSL) on the nginx vhost |
| `discoverable` | `false` | Accept incoming connections from the server (reverse-proxy mode) |
| `listenAddr` | `127.0.0.1` | Bind address for the worker listener |
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
| `settings.maxNixdaemonConnections` | `8` | Worker's local nix-daemon connection pool size |
| `settings.maxProtoConnections` | `16` | Max simultaneous WebSocket connections (for discoverable mode) |
| `settings.logLevel.default` | `info` | Worker log level |
| `settings.logLevel.eval` | null | Evaluator log level override |
| `settings.logLevel.build` | null | Builder log level override |
| `settings.logLevel.proto` | null | Protocol log level override |

## Declarative State

Users, organizations, projects, integrations, caches, API keys, and workers can be declared in `services.gradient.state` and reconciled on every startup. See [Declarative State](state.md) for the full options reference.
