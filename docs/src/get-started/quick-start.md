# Quick Start

A running Gradient instance with its own worker, on one NixOS host.

**Requirements:**

- A NixOS host configured through a flake
- A domain pointing at that host, for example `gradient.example.com`

## 1. Flake Input

```nix
# flake.nix
{
  inputs.gradient.url = "github:wavelens/gradient";

  outputs = { nixpkgs, gradient, ... }: {
    nixosConfigurations.gradient = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        ./configuration.nix
        gradient.nixosModules.default # (1)!
      ];
    };
  };
}
```

1.  Brings in the `services.gradient` options for the server, the worker and the declarative state.

## 2. Create Secrets

```sh
install -d -m 700 /var/lib/gradient-secrets
openssl rand -base64 48 > /var/lib/gradient-secrets/jwt
openssl rand -base64 48 > /var/lib/gradient-secrets/crypt
```

The first signs login sessions, the second encrypts secrets stored in the database. The worker creates its own token and needs nothing here.

!!! tip
    For a production host, manage these files with [sops-nix](https://github.com/Mic92/sops-nix) or [agenix](https://github.com/ryantm/agenix).

## 3. Enable Gradient

```nix
# configuration.nix
{ pkgs, ... }:
{
  services.gradient = {
    enable = true;
    domain = "gradient.example.com"; # (1)!
    secrets.jwtFile = "/var/lib/gradient-secrets/jwt";
    secrets.cryptFile = "/var/lib/gradient-secrets/crypt";
    postgres.enable = true; # (2)!
    worker.enable = true; # (3)!
    sentry.enable = true; # (4)!
  };

  services.postgresql.package = pkgs.postgresql_18; # (5)!
}
```

1.  Served by an nginx virtual host, set up by the module.
2.  A local PostgreSQL database for Gradient.
3.  A worker on the same host. Self-registers and joins every project.
4.  Optional. Sends crash reports to the Gradient developers.
5.  Gradient needs PostgreSQL 18 or newer; the NixOS default may be older.

## Verify Deployment

- The login page loads at `https://gradient.example.com`.
- `systemctl status gradient-server gradient-worker` shows both services running.

The worker stays in a reconnect loop until the first project with a cache exists. That is expected; the next page creates both.

## Next Steps

- [First Project](first-project.md): a project, a cache and the first green build
- [Installation](installation.md): TLS, reverse proxies and production setup
- [Configuration](../reference/configuration.md): every server and worker option
