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
        gradient.nixosModules.gradient-worker
      ];
    };
  };
}
```

## 2. Generate Secrets

Pick a UUID for the worker — you will use it in the config and when registering the worker to your org:

```sh
uuidgen
```

Generate all secret files:

```sh
# Server secrets
openssl rand -base64 48 > /run/secrets/gradient-jwt
openssl rand -base64 48 > /run/secrets/gradient-crypt

# Worker token
openssl rand -base64 48 > /run/secrets/gradient-worker-token

# Peers file: one entry per line in the format <peer_id>:<token>.
# * as peer_id means the worker responds with this token for any org's challenge.
echo "*:$(cat /run/secrets/gradient-worker-token)" > /run/secrets/gradient-worker-peers
```

!!! tip
    Use [sops-nix](https://github.com/Mic92/sops-nix) or [agenix](https://github.com/ryantm/agenix) to manage secrets in production.

## 3. NixOS Configuration

```nix
{
  services.gradient = {
    enable            = true;
    frontend.enable   = true;
    domain            = "gradient.example.com";
    jwtSecretFile     = "/run/secrets/gradient-jwt";
    cryptSecretFile   = "/run/secrets/gradient-crypt";
    configurePostgres = true;
    configureNginx    = true;
    serveCache        = true;
    reportErrors      = true; # optional: will send crash reports to us
  };

  services.gradient.worker = {
    enable    = true;
    serverUrl = "ws://127.0.0.1:3000/proto";
    workerId  = "<uuid from uuidgen>"; # if not provided, a random UUID will be generated and saved in /var/lib/gradient/worker-id
    peersFile = "/run/secrets/gradient-worker-peers";
    capabilities = {
      fetch = true;
      eval  = true;
      build = true;
    };
  };
}
```

After `nixos-rebuild switch`, navigate to `https://gradient.example.com/account/register` to create the first user, then create an organization. Register the worker under **Organization Settings → Workers** using the UUID from step 2 and the token from `/run/secrets/gradient-worker-token`.

## Next Steps

- [Configuration](configuration.md) — full options reference, OIDC, GitHub App, remote workers
- [Usage](usage/overview.md) — evaluation wildcards, SSH keys, triggering builds
- [API Reference](https://petstore.swagger.io/?url=https://raw.githubusercontent.com/wavelens/gradient/master/docs/gradient-api.yaml)
