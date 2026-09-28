# Add a Remote Worker

A second machine that evaluates and builds for a project, next to or instead of the local worker.

**Requirements:**

- A NixOS machine with network access to the Gradient server
- A project with a cache subscription, see [First project](../get-started/first-project.md)

## 1. Pick a Worker ID

Every worker has a UUID. Generate one:

```sh
cat /proc/sys/kernel/random/uuid
```

## 2. Register the Worker

=== "UI"

    In the project, **Settings -> Workers -> Register Worker**, then enter a name and the worker ID from step 1.

    The dialog shows a **Peers file entry** once. Store the entry as a secret on the worker machine, e.g. `/run/secrets/gradient-worker-peers`.

=== "Declarative"

    On the server:

    ```nix
    services.gradient.state = {
      projects.acme.id = "<project uuid>"; # (1)!
      workers.builder-1 = {
        worker_id = "<worker id from step 1>";
        projects = [ "acme" ];
        token_file = "/run/secrets/builder-1-token"; # (2)!
        base_worker = false; # (3)!
      };
    };
    ```

    1.  Pinned, so the worker's peers file can name the project before the server first starts.
    2.  Created with `openssl rand -base64 48`; the same token goes into the worker's peers file.
    3.  Declared workers are base workers by default.

    The peers file entry on the worker machine is `<project uuid>:<token>`.

## 3. Configure the Machine

```nix
# configuration.nix on the worker machine
{
  imports = [ inputs.gradient.nixosModules.default ];

  services.gradient.worker = {
    enable = true;
    id = "<worker id from step 1>";
    serverUrl = "wss://gradient.example.com/proto";
    peersFile = "/run/secrets/gradient-worker-peers"; # (1)!
    build.metrics = true; # (2)!
  };
}
```

1.  One line per project the worker serves, see [Peers File](#peers-file).
2.  Optional. Records memory and CPU per build, so the scheduler places heavy builds on machines that fit them.

## Verify Deployment

- **Settings -> Workers** lists the worker as connected, with the detected systems and features.
- The next evaluation of the project shows builds on the new worker.

!!! tip "One machine, many projects"
    Register the same worker ID in each project and add each project's peers file entry as its own line. For a worker every project may use, declare a [base worker](../usage/state.md#base-workers) instead.

## Peers File

The worker authenticates with one line per project:

```text
# /run/secrets/gradient-worker-peers
<project uuid>:<token>
<other project uuid>:<other token>
```

A single `*:<token>` line answers every project with the same token instead:

| | One line per project | `*:<token>` |
|---|---|---|
| Setup | A new line for every project | One line; each project registers the worker with the same pre-generated token |
| Leaked token | Exposes one project | Exposes every project the worker serves |
| Revoking | Per project | Everywhere at once |
| Fits | Workers shared between teams | A worker owned by one team, serving all of that team's projects |

Prefer one line per project; use `*` only when every project behind the worker has the same owner.

## Next Steps

- [Workers](../concepts/workers.md): capabilities, matching and access
- [Worker options](../configuration.md#worker-options): every worker setting
