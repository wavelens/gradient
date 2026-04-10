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

`configurePostgres` creates a local PostgreSQL database and user. `configureNginx` adds a virtual host that proxies `/api/` and `/cache/` to the backend and serves the frontend SPA.

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
| `settings.maxConcurrentEvaluations` | `1` | Simultaneous Nix evaluations |
| `settings.maxConcurrentBuilds` | `1` | Simultaneous builds across all servers |
| `settings.logLevel` | `info` | Log level: `trace` `debug` `info` `warn` `error` |
| `settings.enableRegistration` | `true` | Allow new user self-registration |
| `settings.deleteState` | `true` | Remove entities no longer in `state` (see below) |

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

## Declarative State

`services.gradient.state` lets you declare users, organizations, projects, servers, caches, and API keys in Nix. Gradient reconciles this state on every startup.

When `settings.deleteState = true` (default), entities that are removed from `state` are also deleted from the database. Set it to `false` to make them editable by users in the frontend instead.

### Users

```nix
services.gradient.state.users = [
  {
    username      = "alice";
    name          = "Alice";
    email         = "alice@example.com";
    password_file = "/run/secrets/alice-password";
    email_verified = true;
  }
];
```

The password file must contain an **argon2id PHC hash**. Generate one with:

```sh
nix shell nixpkgs#libargon2 -c \
  sh -c 'argon2 "$(openssl rand -hex 16)" -id -e <<< "mypassword"' \
  > /run/secrets/alice-password
```

### Organizations

```nix
services.gradient.state.organizations = [
  {
    name             = "acme";
    display_name     = "ACME Corp";
    private_key_file = "/run/secrets/acme-ssh-key";
    created_by       = "alice";
  }
];
```

The SSH private key is the organization key used to clone repositories and authorize build servers. Generate one with:

```sh
ssh-keygen -t ed25519 -N "" -f /run/secrets/acme-ssh-key
# Add the public key (.pub) to your Git host and build servers' authorized_keys
```

### Projects

```nix
services.gradient.state.projects = [
  {
    name                = "web-app";
    organization        = "acme";
    repository          = "git@github.com:acme/web-app.git";
    evaluation_wildcard = "packages.x86_64-linux.*";
    created_by          = "alice";
  }
];
```

### Servers

```nix
services.gradient.state.servers = [
  {
    name          = "builder-1";
    organization  = "acme";
    host          = "build1.internal.example.com";
    username      = "gradient";
    architectures = [ "x86_64-linux" ];
    features      = [ "big-parallel" ];
    created_by    = "alice";
  }
];
```

### Caches

```nix
services.gradient.state.caches = [
  {
    name            = "main";
    signing_key_file = "/run/secrets/cache-signing-key";
    organizations   = [ "acme" ];
    created_by      = "alice";
  }
];
```

Generate a Nix cache signing key with:

```sh
nix-store --generate-binary-cache-key main-cache \
  /run/secrets/cache-signing-key \
  /run/secrets/cache-signing-key.pub
```

### API Keys

```nix
services.gradient.state.api_keys = [
  {
    name     = "ci-token";
    key_file = "/run/secrets/ci-api-key";
    owned_by = "alice";
  }
];
```

The key file must contain a token with the `GRAD` prefix:

```sh
echo "GRAD$(openssl rand -hex 32)" > /run/secrets/ci-api-key
```
