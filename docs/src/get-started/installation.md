<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# Installation

A production instance with managed secrets, a chosen database, and TLS matching the network in front of the server. The steps below extend the [Quick Start](quick-start.md) configuration.

**Requirements:**

- A NixOS host configured through a flake, with the Gradient module added ([Quick Start, step 1](quick-start.md#1-flake-input))
- PostgreSQL 18 or newer. The server will refuse to start against older versions.

## 1. Public Binary Cache

Pre-built Gradient packages are available for substituting instead of compiling.

```nix
nix.settings = {
  substituters = [ "https://public.gradient.ci/cache/main" ];
  trusted-public-keys = [
    "public.gradient.ci-main:qmxRE+saUvhNa3jqaCMWje+feVU77TjABchZrPGf7A8="
  ];
};
```

## 2. Secrets

Gradient needs two secrets, each 48 random bytes in base64.

- `jwtFile`: key for signing login sessions.
- `cryptFile`: key for encrypting secrets stored in the database.

=== "sops-nix"

    ```nix
    sops.secrets.gradient-jwt = { };
    sops.secrets.gradient-crypt = { };

    services.gradient.secrets = {
      jwtFile = config.sops.secrets.gradient-jwt.path;
      cryptFile = config.sops.secrets.gradient-crypt.path;
    };
    ```

=== "agenix"

    ```nix
    age.secrets.gradient-jwt.file = ./secrets/gradient-jwt.age;
    age.secrets.gradient-crypt.file = ./secrets/gradient-crypt.age;

    services.gradient.secrets = {
      jwtFile = config.age.secrets.gradient-jwt.path;
      cryptFile = config.age.secrets.gradient-crypt.path;
    };
    ```

=== "Plain files"

    ```sh
    install -d -m 700 /var/lib/gradient-secrets
    openssl rand -base64 48 > /var/lib/gradient-secrets/jwt
    openssl rand -base64 48 > /var/lib/gradient-secrets/crypt
    ```

    ```nix
    services.gradient.secrets = {
      jwtFile = "/var/lib/gradient-secrets/jwt";
      cryptFile = "/var/lib/gradient-secrets/crypt";
    };
    ```

Root can keep ownership of the files. The server will read them as systemd credentials.

!!! warning
    Every secret stored in the database is unreadable without `cryptFile`. Back up `cryptFile` together with the database.

## 3. Database

=== "Local"

    ```nix
    services.gradient.postgres.enable = true;
    services.postgresql.package = pkgs.postgresql_18;
    ```

    A PostgreSQL 18 cluster on the same host, with a `gradient` role and database.

=== "External"

    ```nix
    services.gradient.database.urlFile = "/run/secrets/gradient-database-url";
    ```

    The file must hold a connection URL such as `postgresql://gradient:<password>@db.example.com/gradient`.

## 4. Reverse Proxy and TLS

The server will handle the API, the worker protocol and the cache. A reverse proxy in front will deliver the web frontend and TLS.

=== "nginx (default)"

    Enabled by default, with a Let's Encrypt certificate.

    ```nix
    services.gradient.domain = "gradient.example.com";
    ```

=== "Caddy"

    ```nix
    services.gradient.reverseProxy.caddy = {
      enable = true; # (1)!
      useACMEHost = "gradient.example.com"; # (2)!
    };
    ```

    1.  Used instead of nginx.
    2.  An existing certificate from `security.acme.certs`. Caddy will request no certificate for this host.

=== "TLS terminated upstream"

    A load balancer or Cloudflare will terminate TLS and forward plain HTTP.

    ```nix
    services.gradient.reverseProxy.nginx.manageTls = false; # (1)!
    ```

    1.  nginx will no longer request a certificate. `useTls` will stay `true`, and Gradient will still emit `https://` links and secure cookies.

=== "Own proxy"

    ```nix
    services.gradient.reverseProxy.nginx.enable = false;
    ```

    | Path | Target |
    |---|---|
    | `/api/`, `/proto`, `/cache/` | Gradient server, with WebSocket upgrades and no request or response buffering |
    | everything else | static files from `${pkgs.gradient-frontend}/share/gradient-frontend` |

!!! warning
    `services.gradient.useTls = false` is only for plain HTTP end to end. Browsers then drop the session cookie behind any HTTPS proxy, and login will break.

## 5. Workers

- Server host: `services.gradient.worker.enable = true`, as in the [Quick Start](quick-start.md#3-enable-gradient).
- Other machines: [Add a Remote Worker](../guides/remote-worker.md).

## Verify Deployment

- `https://gradient.example.com` shows the login page with a valid certificate.
- `journalctl -u gradient-server` shows no database or secret errors.

??? note "Crash Reports"
    `services.gradient.sentry.enable = true` will send crash reports to the Gradient developers. Crash reports are off by default.

??? note "Network Tuning for Fast or Distant Links"
    A default deployment is fine without tuning. BBR and larger TCP buffers help on a high-bandwidth or high-latency link between workers and server.

    ```nix
    boot.kernel.sysctl = {
      "net.ipv4.tcp_congestion_control" = "bbr";
      "net.ipv4.tcp_rmem" = "4096 131072 16777216";
      "net.ipv4.tcp_wmem" = "4096 16384 16777216";
    };
    ```

    Jumbo frames save little CPU. Jumbo frames can hang large transfers on an MTU mismatch at any hop. Enable them only where every hop is under control.

## Next Steps

- [First Project](first-project.md): the first user, project and build
- [Add a Remote Worker](../guides/remote-worker.md): build machines beyond the server host
- [Configuration](../reference/configuration.md): every server and worker option
- [Options search](https://wavelens.github.io/gradient-search): all NixOS options
