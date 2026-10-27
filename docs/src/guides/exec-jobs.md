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

## 2. Declare an App

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

## 3. Ask for Hardware

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
| `labels` | `[ ]` | The worker must carry every label. Worker IDs act as labels too |
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

- Workers can restrict every field. Requests outside the restriction rule that worker out. Downgrades of a request never happen.
- Lost workers lead to a restart without using a retry. Ten losses in a row fail the job.
- Not available yet: `nodes` above 1, `secrets`, SSH sessions, outputs and phase markers in the UI.

## Verify Deployment

- Start an evaluation of the task. Exec jobs appear with a status in the **Exec Jobs** section of the evaluation.
- Waiting jobs name the missing requirement, for example a device without a connected worker.
- Job pages show attempts, the log and an **Abort** button.

## Next Steps

- [Workers](../concepts/workers.md): capabilities and registrations
- [Evaluation Wildcards](../reference/wildcards.md): select the apps of a flake
- [Declarative State](../reference/state.md): `exec_enabled` and `enable_exec`
- [Configuration](../reference/configuration.md): `exec.enable`
