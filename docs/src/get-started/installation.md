# Installation

A production instance: managed secrets, a chosen database, and TLS that fits the network in front of the server. Builds on the [Quick start](quick-start.md) configuration.

**Requirements:**

- A NixOS host configured through a flake, with the Gradient module added ([Quick start, step 1](quick-start.md#1-flake-input))
- PostgreSQL 18 or newer; the server refuses to start against older versions

## 1. Public binary cache

Pre-built Gradient packages, so the host substitutes instead of compiling:

```nix
nix.settings = {
  substituters = [ "https://public.gradient.ci/cache/main" ];
  trusted-public-keys = [
    "public.gradient.ci-main:qmxRE+saUvhNa3jqaCMWje+feVU77TjABchZrPGf7A8="
  ];
};
```

## 2. Secrets

Two secrets, each 48 random bytes in base64: `jwtFile` signs login sessions, `cryptFile` encrypts secrets stored in the database.

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
    openssl rand -base64 48 > /run/secrets/gradient-jwt
    openssl rand -base64 48 > /run/secrets/gradient-crypt
    ```

The files can stay owned by root: the server reads them as systemd credentials.

!!! warning
    Losing `cryptFile` makes every secret stored in the database unreadable. Back up `cryptFile` together with the database.

## 3. Database

=== "Local"

    ```nix
    services.gradient.postgres.enable = true;
    ```

    A PostgreSQL cluster on the same host, with a `gradient` role and database.

=== "External"

    ```nix
    services.gradient.database.urlFile = "/run/secrets/gradient-database-url";
    ```

    The file holds a connection URL such as `postgresql://gradient:<password>@db.example.com/gradient`.

## 4. Reverse proxy and TLS

The server serves the API, the worker protocol and the cache; a reverse proxy in front serves the web frontend and TLS.

=== "nginx (default)"

    Enabled by default, with a Let's Encrypt certificate:

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

    1.  Replaces nginx.
    2.  An existing certificate from `security.acme.certs`; Caddy requests none for this host.

=== "TLS terminated upstream"

    A load balancer or Cloudflare terminates TLS and forwards plain HTTP:

    ```nix
    services.gradient.reverseProxy.nginx.manageTls = false; # (1)!
    ```

    1.  nginx stops requesting a certificate. `useTls` stays `true`, so Gradient still emits `https://` links and secure cookies.

=== "Own proxy"

    ```nix
    services.gradient.reverseProxy.nginx.enable = false;
    ```

    | Path | Target |
    |---|---|
    | `/api/`, `/proto`, `/cache/` | Gradient server, with WebSocket upgrades and no request or response buffering |
    | everything else | static files from `${pkgs.gradient-frontend}/share/gradient-frontend` |

!!! warning
    `services.gradient.useTls = false` is only for plain HTTP end to end. Behind any HTTPS proxy, browsers then drop the session cookie and login breaks.

## 5. Workers

- On the server host: `services.gradient.worker.enable = true`, as in the [Quick start](quick-start.md#3-enable-gradient).
- On other machines: see [Remote workers](../configuration.md#remote-workers).

## Verify Deployment

- `https://gradient.example.com` shows the login page with a valid certificate.
- `journalctl -u gradient-server` shows no database or secret errors.

??? note "Network tuning for fast or distant links"
    A default deployment needs no tuning. On a high-bandwidth or high-latency link between workers and server, BBR and larger TCP buffers help:

    ```nix
    boot.kernel.sysctl = {
      "net.ipv4.tcp_congestion_control" = "bbr";
      "net.ipv4.tcp_rmem" = "4096 131072 16777216";
      "net.ipv4.tcp_wmem" = "4096 16384 16777216";
    };
    ```

    Jumbo frames save little CPU and hang large transfers when any hop disagrees on the MTU; enable them only where every hop is under control.

## Next steps

- [First project](first-project.md): the first user, project and build
- [Remote workers](../configuration.md#remote-workers): build machines beyond the server host
- [Configuration](../configuration.md): every server and worker option
- [Options search](https://wavelens.github.io/gradient-search): all NixOS options
