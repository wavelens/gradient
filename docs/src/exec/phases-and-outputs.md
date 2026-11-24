<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# Report Phases and Upload Outputs

Restart points and result files from within an exec job.

**Requirements:**

- An exec app, see [Declare an Exec App](apps.md)

## 1. Report Phases

Phases show progress in the job log. A resumed attempt can start at the last phase.

```nix
# flake.nix
apps.x86_64-linux.train = {
  type = "app";
  program = "${pkgs.writeShellScript "train" ''
    start=1
    [ -n "$GRADIENT_PHASE" ] && start=$(cat "$GRADIENT_PHASE_PAYLOAD_FILE") # (1)!
    for epoch in $(seq "$start" 10); do
      python train.py --epoch "$epoch"
      echo $((epoch + 1)) > next-epoch
      gradient-phase "epoch-$epoch" next-epoch # (2)!
    done
  ''}";
  passthru.gradient = { devices = [ "gpu:h100" ]; retries = 2; };
};
```

1. Restarted attempts read the last phase name from `GRADIENT_PHASE`. Those attempts read the payload from `GRADIENT_PHASE_PAYLOAD_FILE`.
2. The command form is `gradient-phase <name> [payload-file]`. A payload file is optional.

- Job logs include a boundary for each phase. Raw logs contain line `::gradient-phase:: <name>`.
- Phases trigger event `exec_job.phase`. Details appear in [Events and Webhooks](../reference/events.md).
- Names contain 1 to 128 printable characters. Payloads contain up to 64 KiB.
- Larger payloads fail with `phase payload is <n> bytes, the limit is 65536` and exit with code 1.

## 2. Upload Outputs

```sh
gradient-upload model.bin model/weights.bin
```

- Files from `gradient-upload <file> [path]` land under the job. Calls without a path use the file name.
- Paths are relative and `/`-separated, with at most 1024 bytes and no `.` or `..` segment.
- Job pages show every output and its size, live during the upload. Each path links to the download.
- Downloads accept range requests. `curl -C -` can resume a large download, see [API](../reference/api.md).
- Outputs go away with the evaluation of the job.
- Binary outputs stay as uploaded. Secret masks apply to `gradient/continue.json` and `gradient/tree.patch` only.

| Reserved path | Content |
|---|---|
| `gradient/tree.patch` | Diff of `/work` against the evaluated commit after the program, new untracked files included. Only Gradient can write the file, see [Open Pull Requests](pull-requests.md) |
| `gradient/continue.json` | Entry points for the running evaluation, see [Chain Evaluations](chain-evaluations.md) |

## 3. Metrics { #metrics }

The job page has a **Metrics** view with CPU, RAM and disk use of the guest over the time since the program started. The view follows the attempt and node picked in the sidebar. Use the peaks to size the guest.

- Tiles show the peak against the usable size and against the requested size. The charts show the average and the peak per time step.
- CPU is the number of CPUs in use, for example 1.7 of 2 and not a percentage.
- RAM is the memory the guest uses without the page cache, because the kernel can free the cache at any time.
- Disk is the use of `/work`.
- The guest samples every second and sends a window every 10 seconds and a last partial window when the program ends. Charts hold at most 300 points.
- Usable size is the memory and disk the guest reports and requested size is the guest size of the job.
- Workers and guest agents too old to send usage windows produce no metrics and the view stays empty.
- Metrics never fail a job.
- The same data is available in the API, see [API](../reference/api.md).

## Verify Deployment

- Job logs show the phase boundaries.
- Phases appear in the **Timeline** of the job page, each one lasting until the next.
- Outputs appear on the job page with their size.

## Next Steps

- [Open Pull Requests](pull-requests.md): changes in `/work` as a pull request
- [Chain Evaluations](chain-evaluations.md): `gradient/continue.json`
- [Events and Webhooks](../reference/events.md): `exec_job.phase`
