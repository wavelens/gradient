# Pull Deployment

Gradient supports *pull deployment*, which allows a target machine to periodically fetch and apply a built NixOS configuration from the Gradient server. This is ideal for:

- Low-power devices that cannot run builds themselves
- Systems that should apply updates without disrupting active work
- Air-gapped or firewalled machines that can reach the Gradient server outbound but not vice versa

## How It Works

1. The `gradient-deploy` systemd service on the target machine reads the task's newest evaluation and looks for the entry point whose output path names a system for this host.
2. If that build has finished, the closure is fetched from the integrated Nix cache and switched to. If it is still running, the service waits (see below).
3. If the target already runs the evaluated system, the service stops immediately, whatever the build is doing.

By default the service runs daily at **04:00** via a systemd timer.

## Waiting For CI

The newest commit is usually still building when the timer fires, so by default
the service waits for it rather than reporting that nothing was built.

It follows the task's live WebSocket, `/api/v1/tasks/<project>/<task>/live`, and
re-checks on every event, so it reacts as soon as the build finishes without
polling the API. The wait ends when:

- the system closure for this host is built, and is switched to;
- that build fails, or the whole evaluation fails or is aborted;
- the target already runs the evaluated system.

An entry point's output path is written at evaluation time, so the deployment is
recognised before its build starts. That is what lets an already-up-to-date
target exit at once instead of sitting through a build it does not need.

None of these outcomes fail the unit; each is reported to the journal and exits
successfully. Waiting itself is unbounded, so a run that outlives its own timer
just makes systemd skip the next trigger.

The target therefore needs outbound WebSocket access to the Gradient server, not
only plain HTTP. Set `waitForBuild = false` to restore the old behaviour of
exiting as soon as nothing is built yet.

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
    task     = "myproject/mytask";
  };
}
```

| Option | Default | Description |
|---|---|---|
| `server` | | URL of your Gradient instance |
| `apiKeyFile` | | Path to a file containing an API key with read access to the task |
| `task` | | `project/task` slug to watch |
| `deployFor` | hostname | Name of the deployment configuration to apply |
| `waitForBuild` | `true` | Wait for an in-flight evaluation instead of exiting when nothing is built yet |
| `idleRecheckSec` | `300` | Failsafe re-check interval while waiting, in seconds. Bounds how long a dropped event can stall a deployment; events drive the normal path |
| `dates` | `"04:00"` | When the timer fires, in `systemd.time(7)` format |
| `randomizedDelaySec` | `"0"` | Randomized delay added before each run |

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
