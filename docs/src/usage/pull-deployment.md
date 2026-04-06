# Pull Deployment

Gradient supports *pull deployment*, which allows a target machine to periodically fetch and apply a built NixOS configuration from the Gradient server. This is ideal for:

- Low-power devices that cannot run builds themselves
- Systems that should apply updates without disrupting active work
- Air-gapped or firewalled machines that can reach the Gradient server outbound but not vice versa

## How It Works

1. A build completes successfully on the Gradient server, producing a NixOS system closure.
2. The `gradient-deploy` systemd service on the target machine polls the Gradient API for the latest successful build of a configured project.
3. When a new build is found, the closure is fetched from the integrated Nix cache and `nixos-rebuild switch` is run.

By default the service runs daily at **04:00** via a systemd timer.

## Setup

### 1. Enable the Deploy Module

Add `gradient.nixosModules.deploy` to the target machine's NixOS configuration:

```nix
{
  inputs.gradient.url = "github:wavelens/gradient";

  outputs = { self, nixpkgs, gradient, ... }: {
    nixosConfigurations.mymachine = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        ./configuration.nix
        gradient.nixosModules.deploy
      ];
    };
  };
}
```

### 2. Configure the Service

```nix
{
  system.gradient-deploy = {
    enable      = true;
    server      = "https://gradient.example.com";
    apiKeyFile  = "/var/lib/gradient-deploy/api-key";
    project     = "myorg/myproject";
  };
}
```

| Option | Description |
|---|---|
| `server` | URL of your Gradient instance |
| `apiKeyFile` | Path to a file containing an API key with read access to the project |
| `project` | `organization/project` slug to watch |

### 3. Create an API Key

In the Gradient web interface:

1. Go to **Settings → API Keys**.
2. Create a key with read access.
3. Write the key to the path configured in `apiKeyFile`:

```sh
echo -n "grd_..." | sudo tee /var/lib/gradient-deploy/api-key
sudo chmod 600 /var/lib/gradient-deploy/api-key
```

## Manual Update

To trigger a deployment immediately without waiting for the timer:

```sh
sudo gradient-update
```

## Scheduled Timer

The default timer fires daily at 04:00. To change the schedule, override the systemd timer unit:

```nix
{
  systemd.timers.gradient-deploy.timerConfig.OnCalendar = "*-*-* 02:00:00";
}
```
