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
| `secrets`, `sops` | `[ ]` | Secret names and repository sops files, see [Pass Secrets](#secrets) |
| `timeout` | none | Total seconds, capped by the worker |
| `maxSilent` | worker default | Seconds without output, capped by the worker |
| `retries` | `0` | Restarts after a non-zero exit or a timeout, capped by the worker |

- Workers report the labels, devices and limits they allow. Requests outside those limits rule out that worker. Downgrades of a request never happen.
- Lost workers lead to a restart without using a retry. Ten losses in a row fail the job.
- `nodes` above 1 and `image` don't exist yet.

## 5. Read the Job Context

Programs start as the user `gradient` in `/work`, a checkout of the evaluated commit. Context values arrive as environment variables and in `/run/gradient/context.json`.

| Variable | Value |
|---|---|
| `GRADIENT_JOB`, `GRADIENT_EVALUATION`, `GRADIENT_TASK`, `GRADIENT_PROJECT` | IDs of the job and its origin |
| `GRADIENT_PROJECT_NAME`, `GRADIENT_TASK_NAME` | Names of the project and task |
| `GRADIENT_PARENT_JOB` | The exec job that started this evaluation, empty otherwise |
| `GRADIENT_COMMIT`, `GRADIENT_ENTRY_POINT` | Commit and app attribute |
| `GRADIENT_ATTEMPT` | Attempt number, starting at 1 |
| `GRADIENT_PHASE`, `GRADIENT_PHASE_PAYLOAD_FILE` | Last phase of an earlier attempt, see [Use Exec Job Results](exec-job-results.md) |
| `GRADIENT_API`, `GRADIENT_TOKEN` | Server API and the job token of the attempt |
| `GRADIENT_WORKER`, `GRADIENT_ZONE` | Worker ID and zone |
| `GRADIENT_DEVICES`, `GRADIENT_GPU_INDICES` | Device handles in the guest as JSON, GPU indices |
| `GRADIENT_MOUNT_<NAME>` | Path of a mount |
| `GRADIENT_NODE_INDEX`, `GRADIENT_NODE_COUNT`, `GRADIENT_MASTER_ADDR`, `GRADIENT_MASTER_PORT`, `GRADIENT_PEERS` | Cluster values, a single node for now |

- `args` and `env` accept placeholders like `@gradient.job.commit@` or `@gradient.worker.zone@`. Unknown placeholders fail the job.
- Timeout events end the attempt. The log shows `program produced no output for <n>s (maxSilent exceeded)` or `program exceeded wall-clock timeout of <n>s`.

## 6. Pass Secrets { #secrets }

Jobs list secret names in `secrets`. Each name needs exactly one source.

```nix
# flake.nix
passthru.gradient = {
  secrets = [ "wandb-token" "hf-token" "lab-key" ];
  sops = [ ./secrets/train.yaml ]; # (1)!
  args = [ "--token" "@gradient.secret.hf-token@" ];
};
```

1.  Repository files encrypted with sops. Evaluations hand on the ciphertext only.

| Source | Setup | Use |
|---|---|---|
| Project secrets | Project **Settings -> Secrets**, the API or `secret_files` in the state | Values from project admins, the simplest choice |
| Repository sops files | Files listed in `sops`, encrypted for the project recipient | Values versioned with the code, never typed into Gradient |
| Worker sops files | `exec.sops.files` on the worker, see [Workers](../concepts/workers.md#exec-secrets) | Operator values out of reach of the server |

### Project Secrets

=== "UI"

    Open the project **Settings -> Secrets** and click **Set secret**. Values stay write-only. Only project admins can change secrets.

=== "Declarative"

    ```nix
    services.gradient.state.projects.acme.secret_files = {
      wandb-token = "/run/secrets/wandb-token";
    };
    ```

- A script can set a value with `PUT /projects/{project}/secrets/{name}`. See [API](../reference/api.md) for details.
- Names hold 1 to 64 lowercase letters, digits or dashes. Values hold at most 64 KiB.
- Secrets from the state show as managed. UI and API changes to them fail.

### Repository Sops Files

The project **Settings** show the **Sops recipient**, derived from the project SSH key.

```yaml
# .sops.yaml
creation_rules:
  - path_regex: secrets/.*\.(yaml|json)$
    age: age1... # (1)!
```

1.  The recipient from the settings page.

```sh
sops encrypt --in-place secrets/train.yaml
```

- Top-level string values turn into secrets. Files need YAML or JSON, picked by extension.
- RSA project keys have no `age1` recipient. The SSH public key works as the age recipient with sops 3.10 or newer.
- Files with encrypted comments fail. Remove the comments before encrypting.

### Secrets in the Job

| Form | Example |
|---|---|
| File | `/run/gradient/secrets/wandb-token` |
| Variable | `GRADIENT_SECRET_WANDB_TOKEN` |
| Placeholder | `@gradient.secret.wandb-token@` in `args` and `env` |

Jobs with a secret problem fail before the start, without using a retry.

| Message | Cause |
|---|---|
| `secret "<name>" found in two sources: <first>, <second>` | Two sources hold the name |
| `secret "<name>" not found in project secrets, repository sops files or worker sops files` | No source holds the name |
| `sops file <file>: not encrypted for the project key` | A recipient other than the project key |

- Logs, terminals and the console show `[secret:<name>]` in place of a value. Base64 and URL-encoded values also receive masks.
- Values shorter than 8 bytes stay unmasked. Binary outputs and port forwards stay unmasked too.

## 7. Open a Terminal { #ssh-and-terminal }

Jobs with `ssh = true` accept terminal sessions from the browser and over SSH. Exec workers need `exec.allow.ssh = true` for such jobs.

```nix
# flake.nix
apps.x86_64-linux.notebook = {
  type = "app";
  program = "${pkgs.bashInteractive}/bin/bash";
  passthru.gradient = { ssh = true; ports = [ 8888 ]; };
};
```

- Programs of such jobs start on a terminal. `bash` programs wait for input and hold the job up until the shell exits.
- A job with `ssh = false` provides the guest serial console as its only access.
- Sessions need the `TriggerEvaluation` permission on the project, see [Members and Roles](../ui/members-and-roles.md).
- New shells start as `gradient` in `/work` with the environment of the program.
- Interactive programs still obey `maxSilent`. Typed input echoed by the terminal counts as output.

### Terminal Tab

The **Terminal** tab of the job page offers three sessions.

| Session | Job status | Content |
|---|---|---|
| **Console** | `starting` or `running` | Serial console of the guest, starting with the last 64 KiB of output |
| **Attach to program** | `running`, `ssh = true` | The terminal of the program |
| **New shell** | `running`, `ssh = true` | A new shell next to the program |

- The oldest viewer can type into the console. Later viewers see a read-only view.
- Programs stay active past the close of an attached view.

### SSH Logins

SSH logins need the SSH server and a key from [Build over SSH](build-over-ssh.md).

| Login | Target |
|---|---|
| `<project>/<job-id>` | The job |
| `<project>/<job-id>/<node>` | A node of the job, only `0` for now |
| `<project>/<task>` | The newest running exec job of the task |

```sh
ssh -p 2222 myproject/<job-id>@ci.example.com # (1)!
ssh -t -p 2222 myproject/<job-id>@ci.example.com shell # (2)!
ssh -p 2222 -N -L 8888:127.0.0.1:8888 myproject/<job-id>@ci.example.com # (3)!
```

1.  Attach to the terminal of the program.
2.  Open a new shell next to the program.
3.  Forward the local port 8888 to port 8888 inside the guest.

- Job pages show the `ssh` command and an `ssh -N -L` command per port, ready to copy.
- Forwards reach only ports listed in `ports`.
- Logins accept no command except `shell`.
- Sessions close on a worker disconnect. Programs keep going.

## Verify Deployment

- Start an evaluation of the task. Exec jobs appear with a status in the **Exec Jobs** section of the evaluation.
- Waiting jobs name the missing requirement, for example a device without a connected worker.
- Job pages show attempts, the log and an **Abort** button.

## Next Steps

- [Use Exec Job Results](exec-job-results.md): chained evaluations, pull requests, outputs and phases
- [Workers](../concepts/workers.md): registrations and capabilities
- [Evaluation Wildcards](../reference/wildcards.md): select the apps of a flake
- [Declarative State](../reference/state.md): the `exec_enabled` flag, `enable_exec` and `secret_files`
- [Configuration](../reference/configuration.md): the `exec.enable` and `exec.allow.ssh` settings
- [Build over SSH](build-over-ssh.md): SSH server and key setup
