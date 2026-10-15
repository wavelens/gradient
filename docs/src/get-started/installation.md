# Installation

This guide covers a production setup: managed secrets, a database of choice, and TLS that matches the network layer in front of the server. It builds on the [Quick Start](quick-start.md) configuration.

**Requirements:**

- A NixOS host managed by a flake with the Gradient module included ([Quick Start, step 1](quick-start.md#1-flake-input))
- PostgreSQL 18 or newer. Older versions prevent the server from starting.

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

Gradient requires two secrets. Each one must contain 48 random bytes in base64.

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

The server loads these files as systemd credentials. Leave ownership with root.

!!! warning
    Without `cryptFile`, no secret in the database can be recovered. Back up `cryptFile` together with the database.

## 3. Database

=== "Local"

    ```nix
    services.gradient.postgres.enable = true;
    services.postgresql.package = pkgs.postgresql_18;
    ```

    This option runs PostgreSQL 18 on the same machine. A `gradient` role and a matching database get created automatically.

=== "External"

    ```nix
    services.gradient.database.urlFile = "/run/secrets/gradient-database-url";
    ```

    Point this option at a file with a connection URL, for example `postgresql://gradient:<password>@db.example.com/gradient`.

## 4. Reverse Proxy and TLS

The server will handle the API, the worker protocol and the cache. A reverse proxy in front will deliver the web frontend and TLS. The server listens on `127.0.0.1:3000`, and `services.gradient.port` can change the port.

=== "nginx (default)"

    This proxy runs by default and provisions a certificate through Let's Encrypt.

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

    1.  Use Caddy in place of nginx.
    2.  Name of a certificate already defined in `security.acme.certs`. Caddy will request no certificate for this host.

=== "TLS terminated upstream"

    A load balancer or Cloudflare can terminate TLS and pass plain HTTP to Gradient.

    ```nix
    services.gradient.reverseProxy.nginx.manageTls = false; # (1)!
    ```

    1.  With this setting, nginx will request no certificate. `useTls` will stay `true`, and Gradient will still emit `https://` links and secure cookies.

=== "Own proxy"

    ```nix
    services.gradient.reverseProxy.nginx.enable = false;
    ```

    | Path | Target |
    |---|---|
    | `/api/`, `/proto`, `/cache/` | Gradient server, with WebSocket upgrades and no request or response buffering |
    | everything else | static files from `${pkgs.gradient-frontend}/share/gradient-frontend` |

!!! warning
    Set `services.gradient.useTls = false` only for plain HTTP end to end. With an HTTPS proxy in the chain, browsers will drop the session cookie and login will fail.

## 5. Workers

- Server host: `services.gradient.worker.enable = true`, as in the [Quick Start](quick-start.md#3-enable-gradient).
- Other machines: [Add a Remote Worker](../guides/remote-worker.md).

## Verify Deployment

- The login page at `https://gradient.example.com` loads with a valid certificate.
- `journalctl -u gradient-server` shows no errors about the database or secrets.

??? note "Crash Reports"
    Set `services.gradient.sentry.enable = true` to send crash reports to the Gradient developers. This option is off by default.

??? note "Network Tuning for Fast or Distant Links"
    A default deployment needs no tuning. On a high-bandwidth or high-latency link between workers and server, BBR and larger TCP buffers help.

    ```nix
    boot.kernel.sysctl = {
      "net.ipv4.tcp_congestion_control" = "bbr";
      "net.ipv4.tcp_rmem" = "4096 131072 16777216";
      "net.ipv4.tcp_wmem" = "4096 16384 16777216";
    };
    ```

    Jumbo frames save little CPU. An MTU mismatch at any hop can hang large transfers. Enable them only where every hop is under your control.

## Next Steps

- [First Project](first-project.md) walks through the first user, project and build
- [Add a Remote Worker](../guides/remote-worker.md) covers build machines outside the server host
- [Configuration](../reference/configuration.md) lists all server and worker options
- The [options search](https://wavelens.github.io/gradient-search) indexes every NixOS option
