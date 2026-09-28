# Pull Deployment

NixOS machines that fetch and switch to their newest configuration built by Gradient on their own. The machine only needs outbound access to the server; no deploy host pushes.

**Requirements:**

- A task that builds the machine's `nixosConfigurations`, see [First Project](../get-started/first-project.md)
- The machine uses the task's cache as a substituter, see [Share a Cache](share-a-cache.md#1-use-the-cache-on-a-machine)

## 1. Build the System

The task's wildcard selects the system of every machine to deploy:

```text
nixosConfigurations.*.config.system.build.toplevel
```

Each machine picks the output named `nixos-system-<hostname>-...`: the host name in the configuration has to match the machine's `deployFor`.

## 2. Create an API Key

**Settings -> API Keys -> New API Key**, with **Scope** Project set to the task's project. A leaked key then reaches nothing else. Store the key on the machine as a secret, e.g. `/run/secrets/gradient-deploy-key`.

## 3. Enable the Deploy Module

```nix
# flake.nix
nixosConfigurations.office-pc = nixpkgs.lib.nixosSystem {
  modules = [
    ./configuration.nix
    gradient.nixosModules.deploy
  ];
};
```

```nix
# configuration.nix
system.gradient-deploy = {
  enable = true;
  server = "https://gradient.example.com";
  task = "acme/machines"; # (1)!
  apiKeyFile = "/run/secrets/gradient-deploy-key";
  dates = "04:00"; # (2)!
};
```

1.  `<project>/<task>`.
2.  When the timer fires, in `systemd.time(7)` format.

## Verify Deployment

```sh
sudo gradient-update
journalctl -u gradient-deploy
```

`gradient-update` runs the deployment at once, without waiting for the timer. The journal ends with `Deployment to /nix/store/...-nixos-system-office-pc-... completed successfully`, or with the reason no deployment ran.

`... without a deployment for <name>` means no system matched: the `networking.hostName` of the built configuration differs from `deployFor`.

## Run Behavior

Each run reads the task's newest evaluation and decides:

| Newest system for the machine | Result |
|---|---|
| Already running | Stops at once |
| Built | Fetched from the cache and switched to |
| Still building | Waits for the build, then switches |
| Failed, or the evaluation failed | Stops, reported in the journal |

None of these fail the unit. While waiting, the service follows the task's live WebSocket and reacts the moment the build finishes.

| Option | Default | Effect |
|---|---|---|
| `deployFor` | host name | Which `nixos-system-<name>` to deploy |
| `waitForBuild` | `true` | `false` stops at once when the newest system is not built yet |
| `websockets` | `true` | `false` checks every `pollIntervalSec` instead, for networks that block WebSocket upgrades |
| `pollIntervalSec` | `60` | Check interval without WebSockets |
| `randomizedDelaySec` | `"0"` | Spreads the runs of many machines |

## Next Steps

- [Share a Cache](share-a-cache.md): the substituter and netrc on the machine
- [Evaluations and Builds](../concepts/evaluations-and-builds.md): what the machine waits for
