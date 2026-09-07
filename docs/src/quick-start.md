# Quick Start

Minimal setup: Gradient server + co-located worker on a single NixOS host.

## 1. Add the Flake Input

```nix
{
  inputs.gradient.url = "github:wavelens/gradient";

  outputs = { self, nixpkgs, gradient, ... }: {
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

## 2. Generate Secrets

The server needs two secrets. The worker generates its own, so there is nothing
to create for it:

```sh
openssl rand -base64 48 > /run/secrets/gradient-jwt
openssl rand -base64 48 > /run/secrets/gradient-crypt
```

!!! tip
    Use [sops-nix](https://github.com/Mic92/sops-nix) or [agenix](https://github.com/ryantm/agenix) to manage secrets in production.

## 3. NixOS Configuration

```nix
{
  services.gradient = {
    enable                    = true;
    frontend.enable           = true;
    domain                    = "gradient.example.com";
    jwtSecretFile             = "/run/secrets/gradient-jwt";
    cryptSecretFile           = "/run/secrets/gradient-crypt";
    configurePostgres         = true;
    reverseProxy.nginx.enable = true;
    reportErrors              = true; # optional: ships crash reports to upstream Wavelens. Override via `settings.sentryDsn = "your-dsn"`.
  };

  services.gradient.worker = {
    enable = true;
    settings.buildMetrics = true; # opt in to per-build resource metrics for smarter scheduling (enables Nix's cgroups experimental feature)
  };
}
```

A worker on the server's own host registers itself: the module derives its
identity, generates its token, and enables it for every project. See
[Configuration → Co-located Worker](configuration.md#co-located-worker) for what
it provisions and how to opt out.

After `nixos-rebuild switch`, navigate to `https://gradient.example.com/account/register`
to create the first user, then create a project and a cache for it. The worker
shows up under **Project Settings → Workers** already enabled - a brand-new
instance can take up to a minute to show it as connected, because a worker is
refused until a project with a subscribed cache exists.

## Next Steps

- [Configuration](configuration.md) - full options reference, OIDC, GitHub App, remote workers
- [Usage](usage/overview.md) - evaluation wildcards, SSH keys, triggering builds
- [API Reference](https://petstore.swagger.io/?url=https://raw.githubusercontent.com/wavelens/gradient/master/docs/gradient-api.yaml)
