# Configuration

Gradient is configured through environment variables. When using the NixOS module all variables are set automatically from the module options; the table below is useful for manual or Docker-based deployments.

## Server Variables

| Variable | Default | Description |
|---|---|---|
| `GRADIENT_DATABASE_URL` | — | PostgreSQL connection string (required) |
| `GRADIENT_IP` | `127.0.0.1` | IP address to bind the HTTP server |
| `GRADIENT_PORT` | `3000` | Port to bind the HTTP server |
| `GRADIENT_DEBUG` | `false` | Enable verbose debug logging |
| `GRADIENT_MAX_CONCURRENT_EVALUATIONS` | `10` | Maximum simultaneous Nix evaluations |
| `GRADIENT_MAX_CONCURRENT_BUILDS` | `1000` | Maximum simultaneous builds across all servers |

## Secret Variables

| Variable | Description |
|---|---|
| `GRADIENT_CRYPT_SECRET` | Encryption secret (base64-encoded) used to protect stored credentials |
| `GRADIENT_JWT_SECRET` | RS256 JWT signing secret for API tokens |

!!! warning
    Never commit secret values to version control. Use a secrets manager such as [sops-nix](https://github.com/Mic92/sops-nix) or [agenix](https://github.com/ryantm/agenix).

## Authentication

### Basic Authentication (default)

Username and password authentication is enabled by default. No additional configuration is required.

### OIDC / OAuth2

To enable single sign-on via an OIDC provider:

| Variable | Description |
|---|---|
| `GRADIENT_OAUTH_ENABLED` | Set to `true` to enable OAuth2/OIDC |
| `GRADIENT_OAUTH_REQUIRED` | Set to `true` to disable basic auth (OIDC only) |
| `GRADIENT_OAUTH_CLIENT_ID` | OIDC client ID |
| `GRADIENT_OAUTH_CLIENT_SECRET_FILE` | Path to a file containing the client secret |
| `GRADIENT_OIDC_DISCOVERY_URL` | OIDC provider discovery URL, e.g. `https://auth.example.com` |
| `GRADIENT_OAUTH_SCOPES` | Space-separated OAuth scopes (default: `openid email profile`) |

When `GRADIENT_OIDC_DISCOVERY_URL` is set, Gradient automatically discovers the authorization, token, and userinfo endpoints from the provider's `.well-known/openid-configuration`. PKCE is used for enhanced security.

### Legacy OAuth2 (without OIDC discovery)

| Variable | Description |
|---|---|
| `GRADIENT_OAUTH_AUTH_URL` | Authorization endpoint URL |
| `GRADIENT_OAUTH_TOKEN_URL` | Token endpoint URL |
| `GRADIENT_OAUTH_API_URL` | User info endpoint URL |

## NixOS Module Options

The NixOS module exposes all of the above through structured Nix options. The full list is available at the [Options Search](https://wavelens.github.io/gradient-search).

Common options:

```nix
services.gradient = {
  enable                        = true;
  frontend.enable               = true;
  domain                        = "gradient.example.com";
  port                          = 3000;
  serveCache                    = true;
  reportErrors                  = false;
  configurePostgres             = true;
  configureNginx                = true;
  maxConcurrentEvaluations      = 10;
  maxConcurrentBuilds           = 1000;
  cryptSecretFile               = "/run/secrets/gradient-crypt";
  jwtSecretFile                 = "/run/secrets/gradient-jwt";
  oauth = {
    enable          = false;
    required        = false;
    clientId        = "";
    clientSecretFile = "";
    discoveryUrl    = "";
  };
};
```
