# Declare an Exec App

A flake app turned into an exec job, with the hardware and context the program needs.

**Requirements:**

- Exec jobs turned on for the task, see [Turn On Exec Jobs](enable.md)
- An exec worker, see [Set Up an Exec Worker](worker.md)

## 1. Add `passthru.gradient`

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

## 2. Ask for Hardware

```nix
passthru.gradient = {
  labels = [ "lab-a" ];
  cpus = 32;
  memoryMb = 131072;
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
| `cpus` | `2` | Virtual CPUs of the guest, 1 to 255, see [Guest Size](worker.md#guest-size) |
| `memoryMb` | `500` | Guest memory in MB, at least 128 |
| `diskMb` | `2048` | Size of the `/work` disk in MB, at least 64 |
| `nodes` | `1` | Guests on separate workers of one zone, see [Spread a Job over Workers](multi-node.md) |
| `labels` | `[ ]` | The worker must report every label |
| `devices` | `[ ]` | Device classes and counts, held until the job ends |
| `mounts` | worker default | Named host directories of the worker |
| `network` | worker default | Internet access inside the job |
| `ssh` | worker default | SSH access into the job, see [Open a Terminal](terminals.md) |
| `ports` | `[ ]` | Guest ports for forwarding. Port 0 is refused |
| `image` | none | A `nixosConfiguration` as the guest system, see [Boot a Custom Image](custom-images.md) |
| `env`, `args` | empty | Environment and arguments of the program |
| `secrets`, `sops` | `[ ]` | Secret names and repository sops files, see [Pass Secrets](secrets.md) |
| `timeout` | none | Total seconds, capped by the worker |
| `maxSilent` | worker default | Seconds without output, capped by the worker |
| `retries` | `0` | Restarts after a non-zero exit or a timeout, capped by the worker |
| `autostart` | `true` | Start after the program build. `false` waits for **Run**, see [Start by Hand](#manual-start) |

- Workers report the labels, devices and limits they allow. Requests outside those limits rule out that worker. Downgrades of a request never happen.
- Lost workers lead to a restart without using a retry. Ten losses in a row fail the job.

## 3. Start by Hand { #manual-start }

Apps with `autostart = false` get a job without a start after the build.

- Jobs with no start stay in `created` and show the wait reason "waits for a manual start".
- Click **Run** on the job page to start the job. Clicks during the program build wait for the build, then start the job.
- Waiting jobs hold no evaluation in `Building`. An evaluation can complete without waiting jobs.
- Git hosts get no status check for a job without a start.
- One click can start every node of a multi-node job.

## 4. Read the Job Context

There is a checkout of the evaluated commit in `/work`. Programs start as the user `gradient` in that directory. Task URLs with `?dir=<rel-dir>` start the program in `/work/<rel-dir>`, see [Repository URL](../concepts/projects-and-tasks.md#repository-url). Context values arrive as environment variables and in `/run/gradient/context.json`.

| Variable | Value |
|---|---|
| `GRADIENT_JOB`, `GRADIENT_EVALUATION`, `GRADIENT_TASK`, `GRADIENT_PROJECT` | IDs of the job and its origin |
| `GRADIENT_PROJECT_NAME`, `GRADIENT_TASK_NAME` | Names of the project and task |
| `GRADIENT_PARENT_JOB` | The exec job that started this evaluation, empty otherwise, see [Chain Evaluations](chain-evaluations.md) |
| `GRADIENT_COMMIT`, `GRADIENT_ENTRY_POINT` | Commit and app attribute |
| `GRADIENT_ATTEMPT` | Attempt number, starting at 1 |
| `GRADIENT_PHASE`, `GRADIENT_PHASE_PAYLOAD_FILE` | Last phase of an earlier attempt, see [Report Phases](phases-and-outputs.md) |
| `GRADIENT_API`, `GRADIENT_TOKEN` | Server API and the job token of the attempt, see [Call Gradient from the Job](pull-requests.md#job-token) |
| `GRADIENT_WORKER`, `GRADIENT_ZONE` | Worker ID and zone |
| `GRADIENT_DEVICES`, `GRADIENT_GPU_INDICES` | Device handles in the guest as JSON, GPU indices |
| `GRADIENT_MOUNT_<NAME>` | Path of a mount |
| `GRADIENT_NODE_INDEX`, `GRADIENT_NODE_COUNT`, `GRADIENT_MASTER_ADDR`, `GRADIENT_MASTER_PORT`, `GRADIENT_PEERS` | Node index, node count, master address and port, every node as JSON, see [Cluster Context](multi-node.md#cluster-context) |

- `args` and `env` accept placeholders like `@gradient.job.commit@` or `@gradient.worker.zone@`. Unknown placeholders fail the job.
- Timeout events end the attempt. The log shows `program produced no output for <n>s (maxSilent exceeded)` or `program exceeded wall-clock timeout of <n>s`.

## Verify Deployment

- Start an evaluation of the task. Exec jobs appear with a status in the **Exec Jobs** section of the evaluation.
- Waiting jobs name the missing requirement, for example a device without a connected worker.
- Job pages show attempts, the log and an **Abort** button.

## Next Steps

- [Pass Secrets](secrets.md): tokens and keys for the program
- [Open a Terminal](terminals.md): shells and port forwards into the job
- [Report Phases and Upload Outputs](phases-and-outputs.md): restart points and result files
- [Evaluation Wildcards](../reference/wildcards.md): select the apps of a flake
