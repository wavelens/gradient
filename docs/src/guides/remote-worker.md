# Add a Remote Worker

A remote worker is an extra machine that can evaluate and build for a project. It can run alongside the local worker or take over from it.

**Requirements:**

- A NixOS machine with network access to the Gradient server
- A project with a cache subscription, see [First Project](../get-started/first-project.md)

## 1. Pick a Worker ID

A worker ID is a UUID. Generate one with this command:

```sh
cat /proc/sys/kernel/random/uuid
```

## 2. Register the Worker

=== "UI"

    In the project, go to **Settings -> Workers -> Register Worker**. Provide a name and the worker ID from step 1.

    The dialog then shows a **Peers file entry** once. Save that entry as a secret on the worker machine, for example at `/run/secrets/gradient-worker-peers`.

=== "Declarative"

    Configure the server with:

    ```nix
    services.gradient.state = {
      projects.acme.id = "<project uuid>"; # (1)!
      workers.builder-1 = {
        worker_id = "<worker id from step 1>";
        projects = [ "acme" ];
        token_file = "/run/secrets/builder-1-token"; # (2)!
      };
    };
    ```

    1.  The peers file for the worker references this ID. Add the project to the peers file before the server's first start.
    2.  Generate the token with `openssl rand -base64 48`. Put the same token in the worker's peers file.

    On the worker machine, write the peers file entry as `<project uuid>:<token>`.

## 3. Open the Server to Workers

Remote workers use a WebSocket at `/proto` to reach the server. The bundled nginx forwards `/proto` only when `proto.public` is enabled.

```nix
# configuration.nix on the server
services.gradient.proto.public = true;
```

Without this setting, the local worker can still connect on the loopback address.

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

1.  This file holds one line per project on this worker. See [Peers File](#peers-file).
2.  Optional. With this enabled, the worker records memory and CPU usage for each build. The scheduler can use those numbers to place heavy builds on machines with matching capacity.

## Verify Deployment

- Check **Settings -> Workers**. The worker appears as connected, together with the detected systems and features.
- The next project evaluation places builds on the new worker.

!!! tip "One Machine, Many Projects"
    Register the same worker ID in each project. Give each project its own line in the peers file. To share a worker across a whole team, use a [team worker](../concepts/teams.md#team-workers) instead.

## Peers File

The worker authenticates with one line per project.

```text
# /run/secrets/gradient-worker-peers
<project uuid>:<token>
<other project uuid>:<other token>
```

A single `*:<token>` line can cover every project with the same token instead.

| | One line per project | `*:<token>` |
|---|---|---|
| Setup | One new line per project | One line. Every project must register the worker with the same pre-generated token |
| Leaked token | One project exposed | Every project of the worker exposed |
| Revoking | Per project | Everywhere at once |
| Fit | Workers shared between teams | A worker owned by one team, serving all of that team's projects |

Prefer one line per project. Reserve `*` for workers where a single owner holds every project.

A [team worker](../concepts/teams.md#team-workers) needs one line for its team, `<team id>:<token>`, or `*:<token>`.

## Next Steps

- [Workers](../concepts/workers.md): capabilities, matching and access
- [Worker options](../reference/configuration.md#worker): every worker setting
