# Spread a Job over Workers

One exec job with a guest per node, each on its own worker of one zone. All nodes start together or not at all.

**Requirements:**

- An exec app, see [Declare an Exec App](apps.md)
- Exec workers in one [zone](../concepts/workers.md#zones), see [Set Up an Exec Worker](worker.md)

## 1. Set `nodes`

```nix
# flake.nix
passthru.gradient = {
  nodes = 4;
  devices = [ "gpu:h100:8" ];
  args = [ "--master" "@gradient.cluster.master_addr@:@gradient.cluster.master_port@" ]; # (1)!
  keepRunning = false; # (2)!
};
```

1.  Node 0 is the master. All nodes receive the master address and port.
2.  A node failure doesn't stop the other nodes when the value is `true`.

- Multi-node jobs need `network`. The default for a missing `network` is `true`. Apps with `network = false` fail with `nodes > 1 needs network`.
- Every worker needs `exec.allow.nodes` of at least `nodes`, plus `exec.allow.network`.
- All nodes share one zone. Too few matching workers in one zone leave the job waiting with `needs N workers in one zone`.
- Lists and events show the job once, under node 0. All node statuses combine into the job status.

## 2. Read the Cluster Context { #cluster-context }

| Key | Variable | Value |
|---|---|---|
| `node_index` | `GRADIENT_NODE_INDEX` | Index of this node, from 0 |
| `node_count` | `GRADIENT_NODE_COUNT` | Number of nodes |
| `master_addr` | `GRADIENT_MASTER_ADDR` | Guest address of node 0 |
| `master_port` | `GRADIENT_MASTER_PORT` | Port for the master process |
| `peers` | `GRADIENT_PEERS` | Every node as JSON, sorted by index, this node included |
| `attempt` | `GRADIENT_ATTEMPT` | Attempt number, starting at 1 |

```json
[{"index":0,"address":"10.233.0.2","port":29500},{"index":1,"address":"10.234.0.2","port":29500}]
```

- `args` and `env` accept the keys as `@gradient.cluster.<key>@`.
- The master port defaults to 29500. Ports outside `exec.allow.ports` of the master worker or listed in `ports` give way to the lowest free port of that range.

## 3. Connect the Workers

Nodes connect to each other on their guest addresses. The operator provides the network between the exec workers of a zone.

| Need | Setup |
|---|---|
| Own subnet | A distinct `exec.subnet` on every exec worker |
| Routes | A route to the subnet of every other exec worker, via that worker. Static routes or the LAN router both work |
| Forwarding | No drop rule for traffic between the LAN interface and `exec.bridge` |
| Ports | The `exec.allow.ports` range open between the workers |

```nix
# configuration.nix of worker a, LAN address 192.168.1.11
services.gradient.worker.exec.subnet = "10.233.0.0/16";
networking.interfaces.eth0.ipv4.routes = [
  { address = "10.234.0.0"; prefixLength = 16; via = "192.168.1.12"; } # (1)!
];
```

1.  Worker b with `exec.subnet = "10.234.0.0/16"` and a route back to `10.233.0.0/16`.

- Worker NATs can hide a guest's source address. Peers see the address of the other worker as source.
- Two workers on one subnet fail every node with `two nodes have the guest address <address>; give every exec worker its own exec.subnet`.

## 4. Plan for Restarts

| Event | Result |
|---|---|
| A node failed or timed out, without `keepRunning` | The other nodes stop. Every node restarts from its last phase while retries remain, one retry per restart |
| A node failed, with `keepRunning = true` | The other nodes go on. The job ends `failed` after the last node, without a restart |
| A lost worker connection | Every node restarts from its last phase without a retry. A fourth loss fails the job |
| A server restart | Every node restarts from its last phase without a retry. Server restarts stay outside the limit of losses |
| An abort | Every node ends `aborted`. No restart |

- Workers apply caps to the `retries` of their own nodes. Jobs take the lowest of those capped values as their retry count.
- Phases mark the restart point of each node, see [Report Phases](phases-and-outputs.md).

!!! warning
    Nodes never survive a lost worker connection or a server restart. These cases stay the same with `stopGuestsWithWorker = false`. All nodes restart from their last phase.

## Verify Deployment

- The job page shows a **Node** picker with one entry per node.
- Logs follow the selected node.

## Next Steps

- [Workers](../concepts/workers.md#zones): zones
- [Open a Terminal](terminals.md): node logins and the **Node** picker
- [Report Phases and Upload Outputs](phases-and-outputs.md): restart points
