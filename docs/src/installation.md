# Installation

Gradient is distributed as a NixOS module. The recommended way to install it is via the Nix flake.

## Prerequisites

- NixOS with flakes enabled
- PostgreSQL (can be configured automatically)
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

    # Secrets — we recommend sops-nix or agenix
    cryptSecretFile = "/var/lib/gradient/crypt-secret"; # base64-encoded password
    jwtSecretFile   = "/var/lib/gradient/jwt-secret";   # random alphanumeric RS256 secret

    # Convenience options
    configurePostgres = true;
    configureNginx    = true;
    serveCache        = true;
  };
}
```

The server automatically starts a **local `gradient-worker`** connected on the loopback interface with `fetch`, `eval`, `build`, and `sign` capabilities. This worker handles all jobs out of the box for single-host setups. See [Configuration → Workers](configuration.md#workers) to add remote workers or tune the local one.

All available options are searchable at the [Options Search](https://wavelens.github.io/gradient-search).

## Binary Cache (Optional)

Add the public cache to avoid rebuilding Gradient from source:

```nix
{
  nix.settings = {
    substituters     = [ "https://gradient.wavelens.io/cache/main" ];
    trusted-public-keys = [
      "gradient.wavelens.io-main:qmxRE+saUvhNa3jqaCMWje+feVU77TjABchZrPGf7A8="
    ];
  };
}
```

## Applying the Configuration

```sh
sudo nixos-rebuild switch --flake .#yourhostname
```

Gradient will start automatically and be available at `https://gradient.example.com`.

## First Steps After Installation

1. Navigate to `https://gradient.example.com/account/register` to create the first user account.
2. Log in and create an organization.
3. Create a Nix cache (optional — required for binary cache serving).
4. Create your first project pointing to a Git repository.
5. Trigger an evaluation — the local worker will fetch, evaluate, and build automatically.
