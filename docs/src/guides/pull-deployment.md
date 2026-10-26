<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# Pull Deployment

NixOS machines that fetch and switch to their newest configuration built by Gradient on their own. Machines need only outbound access to the server. No deploy host has to push.

**Requirements:**

- A task building the machine's `nixosConfigurations`, see [First Project](../get-started/first-project.md)
- The task's cache as a substituter on the machine, see [Share a Cache](share-a-cache.md#1-use-the-cache-on-a-machine)

## 1. Build the System

Set the task's wildcard to the system attribute path so every machine builds its own NixOS closure:

```text
nixosConfigurations.*.config.system.build.toplevel
```

Each machine resolves to an output named `nixos-system-<hostname>-...`. Keep the host name in the configuration identical to that machine's `deployFor` value.

## 2. Create an API Key

Generate a key under **Settings -> API Keys -> New API Key**. Set **Scope** Project to the project that owns the task, so a leaked key grants no access beyond it. Save the key on the machine as a secret, for example at `/run/secrets/gradient-deploy-key`.

## 3. Enable the Deploy Module

Add the client module to the machine in `flake.nix`:

```nix
# flake.nix
nixosConfigurations.office-pc = nixpkgs.lib.nixosSystem {
  modules = [
    ./configuration.nix
    gradient.nixosModules.client
  ];
};
```

Then turn on deployment in `configuration.nix`:

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

To run a deployment right away instead of waiting for the timer, use `gradient-update`. The last journal lines show either `Deployment to /nix/store/...-nixos-system-office-pc-... completed successfully` or the reason why no deployment ran.

The message `... without a deployment for <name>` means no system matched: the `networking.hostName` of the built configuration did not match `deployFor`.

An `Error:` line in the journal means a failed fetch or switch. In that case no switch happened.

## Run Behavior

Each round evaluates the task's latest evaluation and reacts to the result.

| Latest system on the machine | Action |
|---|---|
| Already running | Immediate stop |
| Built | Fetch from cache and switch |
| Still building | Wait for the build, then switch |
| Failed, or evaluation failed | Stop, report in the journal |
| Not fetchable, or switch failed | `Error:` in the journal, retry after 10 seconds, no switch |

The waiting outcomes keep the unit in a successful state. The waiting service can track the task's live WebSocket. A finished build can turn into an immediate switch.

| Option | Default | Effect |
|---|---|---|
| `deployFor` | host name | Which `nixos-system-<name>` to deploy |
| `waitForBuild` | `true` | `false` stops at once if the latest system has not been built yet |
| `websockets` | `true` | `false` checks every `pollIntervalSec` instead, for networks that block WebSocket upgrades |
| `pollIntervalSec` | `60` | Check interval without WebSockets |
| `randomizedDelaySec` | `"0"` | Spread out deployments across many machines |

## Next Steps

- [Share a Cache](share-a-cache.md): set up the substituter and netrc on the machine
- [Evaluations and Builds](../concepts/evaluations-and-builds.md): learn which build states a machine can wait for
