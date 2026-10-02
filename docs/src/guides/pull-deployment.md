# Pull Deployment

NixOS machines that fetch and switch to their newest configuration built by Gradient on their own. Machines need only outbound access to the server. No deploy host is pushing.

**Requirements:**

- A task building the machine's `nixosConfigurations`, see [First Project](../get-started/first-project.md)
- The task's cache as a substituter on the machine, see [Share a Cache](share-a-cache.md#1-use-the-cache-on-a-machine)

## 1. Build the System

The task's wildcard is selecting the system of every machine to deploy.

```text
nixosConfigurations.*.config.system.build.toplevel
```

Each machine is picking the output named `nixos-system-<hostname>-...`. The host name in the configuration must match the machine's `deployFor`.

## 2. Create an API Key

Open **Settings -> API Keys -> New API Key** and set **Scope** Project to the task's project. A leaked key can then reach nothing else. Store the key on the machine as a secret, e.g. `/run/secrets/gradient-deploy-key`.

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
2.  Timer schedule in `systemd.time(7)` format.

## Verify Deployment

```sh
sudo gradient-update
journalctl -u gradient-deploy
```

`gradient-update` is running the deployment at once, without waiting for the timer. The journal is ending with `Deployment to /nix/store/...-nixos-system-office-pc-... completed successfully`, or with the reason no deployment ran.

`... without a deployment for <name>` is a sign that no system matched. The `networking.hostName` of the built configuration does not match `deployFor`.

## Run Behavior

Each round is reading the task's newest evaluation and picking one of these outcomes.

| Newest system for the machine | Result |
|---|---|
| Already running | Stopping at once |
| Built | Fetched from the cache and switched to |
| Still building | Waiting for the build, then switching |
| Failed, or the evaluation failed | Stopping, reported in the journal |

None of these fail the unit. The waiting service is following the task's live WebSocket. The service is reacting the moment the build is finished.

| Option | Default | Effect |
|---|---|---|
| `deployFor` | host name | Which `nixos-system-<name>` to deploy |
| `waitForBuild` | `true` | `false` is stopping at once while the newest system is not built yet |
| `websockets` | `true` | `false` is checking every `pollIntervalSec` instead, for networks blocking WebSocket upgrades |
| `pollIntervalSec` | `60` | Check interval without WebSockets |
| `randomizedDelaySec` | `"0"` | Spreading the deployments of many machines |

## Next Steps

- [Share a Cache](share-a-cache.md): the substituter and netrc on the machine
- [Evaluations and Builds](../concepts/evaluations-and-builds.md): what the machine is waiting for
