# Installation

Gradient is distributed as a NixOS module. The recommended way to install it is via the Nix flake.

## Prerequisites

- NixOS with flakes enabled
- PostgreSQL 18 or newer (can be configured automatically); the server refuses to start against older versions
- An NGINX reverse proxy (can be configured automatically)

## Adding Gradient to Your Flake

Add Gradient as a flake input and apply the overlay:

```nix
{
  inputs.gradient.url = "github:wavelens/gradient";
  # Optional: pin nixpkgs to match Gradient's
  # inputs.gradient.inputs.nixpkgs.follows = "nixpkgs";

  outputs = { self, nixpkgs, gradient, ... }:
  let
    pkgs = import nixpkgs {
      system = "x86_64-linux";
      overlays = [ gradient.overlays.default ];
    };
  in {
    nixosConfigurations.yourhostname = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        ./configuration.nix
        gradient.nixosModules.default
      ];
    };
  };
}
```

## Minimal NixOS Configuration

In your `configuration.nix`:

```nix
{
  services.gradient = {
    enable        = true;
    frontend.enable = true;
    domain        = "gradient.example.com";

    # Secrets - we recommend sops-nix or agenix
    cryptSecretFile = "/var/lib/gradient/crypt-secret"; # base64-encoded password
    jwtSecretFile   = "/var/lib/gradient/jwt-secret";   # random alphanumeric RS256 secret

    # Convenience options
    configurePostgres         = true;
    reverseProxy.nginx.enable = true;
  };
}
```

The server does **not** start a worker automatically. Setting `services.gradient.worker.enable = true` adds one on the same machine - it registers itself, so no token or UUID is needed - or deploy `gradient-worker` on separate build machines. See [Configuration → Workers](configuration.md#workers) for the full setup.

All available options are searchable at the [Options Search](https://wavelens.github.io/gradient-search).

## TLS configuration

Two independent settings control TLS, and conflating them is a common source of
broken logins:

- `services.gradient.useTls` (default `true`) controls how Gradient itself
  behaves: it emits `https://` URLs (the OIDC redirect URL, `GRADIENT_SERVE_URL`)
  and marks session cookies `Secure`. Set it to `false` **only** for a genuinely
  plaintext-HTTP deployment - turning it off so that nginx stops managing
  certificates will also stop your browser from sending the secure session
  cookie, breaking login.
- `services.gradient.reverseProxy.nginx.manageTls` (default `true`) controls
  whether nginx obtains and serves the certificate itself (it sets the vhost's
  `enableACME` and `forceSSL`). It has no effect when `useTls = false`.

If TLS is terminated by an upstream proxy (Traefik, Cloudflare, a load balancer)
that forwards plain HTTP to nginx, keep `useTls = true` so Gradient still emits
`https://` URLs and secure cookies, and set `manageTls = false` so nginx doesn't
also try to obtain a certificate:

```nix
{
  services.gradient = {
    useTls = true;                          # emit https URLs + secure cookies
    reverseProxy.nginx.enable = true;       # still let nginx serve static files
    reverseProxy.nginx.manageTls = false;   # upstream proxy terminates TLS
  };
}
```

## Binary Cache (Optional)

Add the public cache to avoid rebuilding Gradient from source:

```nix
{
  nix.settings = {
    substituters     = [ "https://public.gradient.ci/cache/main" ];
    trusted-public-keys = [
      "public.gradient.ci-main:qmxRE+saUvhNa3jqaCMWje+feVU77TjABchZrPGf7A8="
    ];
  };
}
```

## Network Tuning (Optional)

Workers and the server share one long-lived WebSocket per connection, carrying
small latency-critical RPCs alongside multi-megabyte NAR chunks. Gradient
disables Nagle's algorithm on every one of those sockets itself, and the NixOS
modules raise nginx's relay buffer for the `/proto` location, so a default
deployment needs no tuning.

On a high-bandwidth or high-latency link, two kernel settings are worth adding:

```nix
{
  boot.kernel.sysctl = {
    "net.ipv4.tcp_congestion_control" = "bbr";
    "net.ipv4.tcp_rmem" = "4096 131072 16777216";
    "net.ipv4.tcp_wmem" = "4096 16384 16777216";
  };
}
```

BBR recovers throughput on paths with any loss, and the larger buffer maxima
let a single connection fill a high bandwidth-delay-product link. Caddy has no
equivalent of nginx's `proxy_buffer_size` for upgraded connections; it relays
them with a fixed internal buffer and needs no configuration.

### Jumbo frames

Jumbo frames need no change to Gradient - MTU is a property of the network, and
the kernel simply negotiates a larger segment size. They are also worth far less
than they look: segmentation offload already hands the NIC large buffers, so
moving from a 1500 to a 9000 byte MTU typically saves low single-digit percent
CPU rather than the 6x fewer packets the arithmetic suggests.

They carry a real risk in exchange. Every hop has to agree, including switches,
bonded interfaces, and any VXLAN or similar overlay that consumes part of the
payload. One hop that disagrees gives a path-MTU black hole, which on a
long-lived connection looks like this: handshakes and small control frames
succeed, large NAR transfers hang until the send timeout, and the job retries
into the same wall.

Enable them only where you control every hop end to end, and after the settings
above.

## Applying the Configuration

```sh
sudo nixos-rebuild switch --flake .#yourhostname
```

Gradient will start automatically and be available at `https://gradient.example.com`.

## First Steps After Installation

1. Navigate to `https://gradient.example.com/account/register` to create the first user account.
2. Log in and create a project.
3. Create a Nix cache (optional - required for binary cache serving).
4. Create your first task pointing to a Git repository.
5. Trigger an evaluation - a connected `gradient-worker` will fetch, evaluate, and build.
