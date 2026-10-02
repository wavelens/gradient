# Quick Start

A running Gradient instance with its own worker, on one NixOS host. [Standalone](standalone.md) is covering a first try without a NixOS host.

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

1.  Source of the `services.gradient` options for the server, the worker and the declarative state.

## 2. Create Secrets

```sh
install -d -m 700 /var/lib/gradient-secrets
openssl rand -base64 48 > /var/lib/gradient-secrets/jwt
openssl rand -base64 48 > /var/lib/gradient-secrets/crypt
```

`jwt` is signing login sessions. `crypt` is encrypting secrets stored in the database. The worker is creating its own token without any file here.

!!! tip
    Manage these files with [sops-nix](https://github.com/Mic92/sops-nix) or [agenix](https://github.com/ryantm/agenix) on a production host.

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
  };

  services.postgresql.package = pkgs.postgresql_18; # (4)!
}
```

1.  An nginx virtual host, set up by the module, is serving this domain.
2.  A local PostgreSQL database for Gradient.
3.  A worker on the same host, self-registering and joining every project.
4.  Gradient is requiring PostgreSQL 18 or newer. The NixOS default may be older.

## Verify Deployment

- The login page is loading at `https://gradient.example.com`.
- `systemctl status gradient-server gradient-worker` is showing both services running.

The worker is staying in a reconnect loop until the first project with a cache is in place. This loop is expected. The next page is creating both.

## Next Steps

- [First Project](first-project.md): a project, a cache and the first green build
- [Installation](installation.md): TLS, reverse proxies and production setup
- [Configuration](../reference/configuration.md): every server and worker option
