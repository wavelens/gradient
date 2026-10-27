# Run Exec Jobs

Flake apps started on workers after each evaluation, with the hardware they ask for.

**Requirements:**

- A task wildcard over the apps, for example `apps.x86_64-linux.*`, see [Evaluation Wildcards](../reference/wildcards.md)
- A worker with the `exec` capability, see [Workers](../concepts/workers.md)

## 1. Turn On Exec Jobs

Exec jobs need three switches. A repository alone can never start code on a worker.

| Switch | Default | Where |
|---|---|---|
| Instance | on | `services.gradient.exec.enable` |
| Project | off | Project **Settings -> Run exec jobs**, or `exec_enabled` in the state |
| Worker | off | `services.gradient.worker.capabilities.exec` and `enable_exec` on the worker registration |

=== "UI"

    Open the project **Settings** and tick **Run exec jobs**. Only project admins can change the switch.

=== "Declarative"

    ```nix
    services.gradient.state.projects.acme.exec_enabled = true;
    services.gradient.state.workers.gpu-box.enable_exec = true;
    ```

Exec apps of a project without the switch fail with a message on the task page.

| Message | Cause |
|---|---|
| `exec jobs are disabled on this instance` | `exec.enable` is off |
| `exec jobs are disabled for this project` | The project switch is off |

## 2. Set Up an Exec Worker

Exec workers start every job in a fresh cloud-hypervisor guest. The host needs `/dev/kvm`.

```nix
# configuration.nix
services.gradient.worker = {
  capabilities.exec = true;
  exec = {
    image = gradient.lib.mkWorkerImage { # (1)!
      modules = [ ./worker-guest.nix ];
    };
    preWarm = 2; # (2)!
    maxPreWarm = 8; # (3)!
    labels = [ "lab-a" ];
    devices = [ # (4)!
      { name = "gpu:h100"; vfio = [ "0000:17:00.0" "0000:18:00.0" ]; }
      { name = "camera"; usb = "1d6b:0002"; }
    ];
    mounts = [ # (5)!
      { name = "datasets"; path = "/srv/datasets"; readOnly = true; }
      { name = "scratch"; path = "/srv/scratch"; perJob = true; }
    ];
    allow = { # (6)!
      network = true;
      devices = [ "gpu:h100" "camera" ];
      mounts = [ "datasets" "scratch" ];
      retries = 2;
    };
    defaults.mounts = [ "scratch" ];
  };
};
```

1.  A NixOS system for the guests. `gradient.guest.cpus`, `gradient.guest.memoryMb`, `gradient.guest.preScript` and `gradient.guest.postScript` tune every guest. Workers without `image` boot a plain guest image.
2.  Guests booted and paused ahead, waiting for a job.
3.  Upper bound for warm guests for upcoming exec jobs on the server. Leave at `0` for a fixed `preWarm`.
4.  Set exactly `vfio`, `dev` or `usb` per device. Bind each VFIO device to `vfio-pci` first.
5.  Guests find a mount at `/mnt/<name>`. `perJob` mounts give every job a fresh directory below `path`.
6.  Workers send `allow` and `defaults` to the server. The server can apply both at placement.

- Networked jobs join the bridge `exec.bridge` with an address from `exec.subnet`. NAT rules on the host connect the bridge to the outside.
- Guests resolve names through a DNS forwarder on the bridge. Addresses in `exec.nameservers` replace the forwarder.
- The full option list is in [Configuration](../reference/configuration.md).

| `exec.stopGuestsWithWorker` | Worker restart |
|---|---|
| `true` (default) | All guests stop. Running jobs restart in a new attempt without using a retry |
| `false` | Guests keep going. The restarted worker can reattach and resume the log of the same attempt |

!!! warning
    Polkit rules from the module let the worker user start and stop `gradient-guest-*` units. Those units run as root. The worker user is root-equivalent on the host.

## 3. Declare an App

Apps with `passthru.gradient` turn into exec jobs. Apps without the block stay plain `nix run` apps.

```nix
# flake.nix
apps.x86_64-linux.check = {
  type = "app";
  program = "${pkgs.writeShellScript "check" "curl -f https://example.org"}";
  passthru.gradient = { network = true; };
};
```

- Workers build the closure of `program` first. Exec jobs can start once the program is in the cache.
- Field errors fail only the affected app, with a message. Other apps and builds of the evaluation stay unaffected.

## 4. Ask for Hardware

```nix
passthru.gradient = {
  labels = [ "lab-a" ];
  devices = [ "gpu:h100:8" "kvm" ]; # (1)!
  mounts = [ "datasets" ];
  network = true;
  env.WANDB_PROJECT = "llm";
  args = [ "--epochs" "10" ];
  timeout = 14 * 24 * 3600;
  maxSilent = 3600;
  retries = 2;
};
```

1.  Format `"<name>[:<count>]"`. Count 1 is the default. A count of 0 is an error.

| Field | Default | Effect |
|---|---|---|
| `labels` | `[ ]` | The worker must report every label |
| `devices` | `[ ]` | Device classes and counts, held until the job ends |
| `mounts` | worker default | Named host directories of the worker |
| `network` | worker default | Internet access inside the job |
| `ssh` | worker default | SSH access into the job |
| `ports` | `[ ]` | Guest ports for forwarding. Port 0 is refused |
| `image` | none | A `nixosConfiguration` as the guest system, built like the program |
| `env`, `args` | empty | Environment and arguments of the program |
| `timeout` | none | Total seconds, capped by the worker |
| `maxSilent` | worker default | Seconds without output, capped by the worker |
| `retries` | `0` | Restarts after a non-zero exit or a timeout, capped by the worker |

- Workers report the labels, devices and limits they allow. Requests outside those limits rule out that worker. Downgrades of a request never happen.
- Lost workers lead to a restart without using a retry. Ten losses in a row fail the job.
- Not available yet: `nodes` above 1, `image`, `secrets`, SSH sessions, outputs and phase markers in the UI.

## 5. Read the Job Context

Programs start as the user `gradient` in `/work`, a checkout of the evaluated commit. Context values arrive as environment variables and in `/run/gradient/context.json`.

| Variable | Value |
|---|---|
| `GRADIENT_JOB`, `GRADIENT_EVALUATION`, `GRADIENT_TASK`, `GRADIENT_PROJECT` | IDs of the job and its origin |
| `GRADIENT_COMMIT`, `GRADIENT_ENTRY_POINT` | Commit and app attribute |
| `GRADIENT_ATTEMPT` | Attempt number, starting at 1 |
| `GRADIENT_PHASE`, `GRADIENT_PHASE_PAYLOAD_FILE` | Last phase of an earlier attempt |
| `GRADIENT_API`, `GRADIENT_TOKEN` | Server API and the job token, empty for now |
| `GRADIENT_WORKER`, `GRADIENT_ZONE` | Worker ID and zone |
| `GRADIENT_DEVICES`, `GRADIENT_GPU_INDICES` | Device handles in the guest as JSON, GPU indices |
| `GRADIENT_MOUNT_<NAME>` | Path of a mount |
| `GRADIENT_NODE_INDEX`, `GRADIENT_NODE_COUNT`, `GRADIENT_MASTER_ADDR`, `GRADIENT_MASTER_PORT`, `GRADIENT_PEERS` | Cluster values, a single node for now |

- `args` and `env` accept placeholders like `@gradient.job.commit@` or `@gradient.worker.zone@`. Unknown placeholders fail the job.
- Timeouts terminate the attempt. with a log line, `program produced no output for <n>s (maxSilent exceeded)` or `program exceeded wall-clock timeout of <n>s`.

## Verify Deployment

- Start an evaluation of the task. Exec jobs appear with a status in the **Exec Jobs** section of the evaluation.
- Waiting jobs name the missing requirement, for example a device without a connected worker.
- Job pages show attempts, the log and an **Abort** button.

## Next Steps

- [Workers](../concepts/workers.md): capabilities and registrations
- [Evaluation Wildcards](../reference/wildcards.md): select the apps of a flake
- [Declarative State](../reference/state.md): `exec_enabled` and `enable_exec`
- [Configuration](../reference/configuration.md): `exec.enable`
