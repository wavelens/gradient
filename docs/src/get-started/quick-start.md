# Quick Start

A running Gradient instance with its own worker, on one NixOS host. [Standalone](standalone.md) has a first try without a NixOS host.

**Requirements:**

- A NixOS host configured through a flake
- A domain pointing at that host, for example `gradient.example.com`

## 1. Flake Input

```nix
# flake.nix
{
  inputs.gradient.url = "github:wavelens/gradient/latest"; # (1)!

  outputs = { nixpkgs, gradient, ... }: {
    nixosConfigurations.gradient = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        ./configuration.nix
        gradient.nixosModules.default # (2)!
      ];
    };
  };
}
```

1.  The flake ref `github:wavelens/gradient` tracks every commit on `main`. Use `github:wavelens/gradient/beta` to include pre-releases.
2.  The flake provides the `services.gradient` options for the server, the worker and the declarative state.

## 2. Create Secrets

```sh
install -d -m 700 /var/lib/gradient-secrets
openssl rand -base64 48 > /var/lib/gradient-secrets/jwt
openssl rand -base64 48 > /var/lib/gradient-secrets/crypt
```

`jwt` is the key for signing login sessions. `crypt` is the key for encrypting secrets stored in the database. The worker will create its own token without any file here.

!!! tip
    Manage these files with [sops-nix](https://github.com/Mic92/sops-nix) or [agenix](https://github.com/ryantm/agenix) on a production host.

## 3. Enable Gradient

Add this to `configuration.nix`:

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

1.  An nginx virtual host from the module, answering on this domain. Only set it to false if you know what you are doing.
2.  A local PostgreSQL database for Gradient.
3.  A worker on the same host, self-registering and joining every project.
4.  Gradient needs PostgreSQL 18 or newer. The NixOS default may be older.

## Verify Deployment

- The login page is reachable at `https://gradient.example.com`.
- `systemctl status gradient-server gradient-worker` shows both services running.

The worker keeps trying to reconnect until a project with a cache exists. This loop is normal at this stage. The next page covers creation of both.

## Next Steps

- [First Project](first-project.md) walks through creating a project, warming the cache and getting the first build to pass
- [Installation](installation.md) covers TLS, reverse proxies and a production-ready setup
- [Configuration](../reference/configuration.md) documents every server and worker option
