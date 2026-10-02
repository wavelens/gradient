# Add a Remote Worker

A second machine evaluating and building for a project, next to or instead of the local worker.

**Requirements:**

- A NixOS machine with network access to the Gradient server
- A project with a cache subscription, see [First Project](../get-started/first-project.md)

## 1. Pick a Worker ID

Every worker is carrying a UUID as its ID. The command below is generating one.

```sh
cat /proc/sys/kernel/random/uuid
```

## 2. Register the Worker

=== "UI"

    Open **Settings -> Workers -> Register Worker** in the project. Enter a name and the worker ID from step 1.

    The dialog is showing a **Peers file entry** once. Store the entry as a secret on the worker machine, e.g. `/run/secrets/gradient-worker-peers`.

=== "Declarative"

    Server configuration:

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

    1.  Pinned for the worker's peers file. The peers file is naming the project before the server's first start.
    2.  Created with `openssl rand -base64 48`. The worker's peers file is holding the same token.
    3.  Declared workers are base workers by default.

    The peers file entry on the worker machine is `<project uuid>:<token>`.

## 3. Open the Server to Workers

Remote workers connect over a WebSocket on `/proto`. The bundled nginx is forwarding `/proto` only with `proto.public` enabled.

```nix
# configuration.nix on the server
services.gradient.proto.public = true;
```

The local worker is connecting on the loopback address without this setting.

## 4. Configure the Machine

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

1.  One line per project on this worker, see [Peers File](#peers-file).
2.  Optional. Recording memory and CPU per build. The scheduler is using the numbers to place heavy builds on machines that fit them.

## Verify Deployment

- **Settings -> Workers** is listing the worker as connected, with the detected systems and features.
- The next evaluation of the project is showing builds on the new worker.

!!! tip "One Machine, Many Projects"
    Register the same worker ID in each project. Add each project's peers file entry as its own line. A [base worker](../reference/state.md#workersname) is the alternative for a worker every project may use.

## Peers File

The worker is authenticating with one line per project.

```text
# /run/secrets/gradient-worker-peers
<project uuid>:<token>
<other project uuid>:<other token>
```

A single `*:<token>` line is answering every project with the same token instead.

| | One line per project | `*:<token>` |
|---|---|---|
| Setup | A new line for every project | One line. Each project is registering the worker with the same pre-generated token |
| Leaked token | Exposing one project | Exposing every project of the worker |
| Revoking | Per project | Everywhere at once |
| Fit | Workers shared between teams | A worker owned by one team, serving all of that team's projects |

Prefer one line per project. Use `*` only for workers whose projects all share one owner.

## Next Steps

- [Workers](../concepts/workers.md): capabilities, matching and access
- [Worker options](../reference/configuration.md#worker): every worker setting
