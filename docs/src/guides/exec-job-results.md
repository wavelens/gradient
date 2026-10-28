# Use Exec Job Results

Phases, files and pull requests from inside an exec job, plus evaluations started by its end.

**Requirements:**

- An exec job, see [Run Exec Jobs](exec-jobs.md)
- An outbound Git host integration for pull requests, see [Connect GitHub](github.md), [Connect Gitea or Forgejo](gitea.md) or [Connect GitLab](gitlab.md)

## 1. Report Phases

Phases mark progress in the job log. Restarted attempts start from the last phase.

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

- Job logs carry a divider for every phase. Raw logs hold the line `::gradient-phase:: <name>`.
- Phases trigger the event `exec_job.phase`. Details live in [Events and Webhooks](../reference/events.md).
- Names hold 1 to 128 printable characters. Payloads hold at most 64 KiB.
- Larger payloads fail with `phase payload is <n> bytes, the limit is 65536` and exit code 1.

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
| `gradient/tree.patch` | Diff of `/work` against the evaluated commit after the program, new untracked files included. Only Gradient can write the file |
| `gradient/continue.json` | Entry points for the running evaluation, see [Chain Evaluations](#5-chain-evaluations) |

## 3. Call Gradient from the Job

Attempts launch with a distinct job token. The `gradient` CLI inside the guest works without a login.

| Place | Content |
|---|---|
| `/home/gradient/.config/gradient/config.toml` | Server, token, the job's project and task as selections |
| `GRADIENT_API`, `GRADIENT_TOKEN` | The same server and token for scripts |
| `GRADIENT_PROJECT_NAME`, `GRADIENT_TASK_NAME` | Names of the job's project and task |

| Job token scope | Access |
|---|---|
| Reads | Own project, its tasks, evaluations, builds and exec jobs |
| Writes | Build requests, evaluations and pull requests of the own task |
| Everything else | `403` with `job tokens only reach their own project and task` |

- Job tokens act with the rights of the user behind the evaluation, else the task creator.
- Tokens end with the attempt. Job pages list each token with its expiry, never its value.
- Logs show the token as `[secret:job-token]`.
- Changed trees reach the workers through `gradient build`, see [Build Before Pushing](build-before-push.md). Job pages link each such evaluation.

## 4. Open Pull Requests

| Way | Pick for |
|---|---|
| `gradient pr open` in the program | Many pull requests per job, each with chosen paths |
| `exec_tree` PR action | One pull request with every change, without a CLI call |

Both ways need an `open_pr` action naming the integration. Tasks without one answer `the task has no open_pr action naming a Git host integration`.

### From the Program

```sh
gradient pr open --title "flake: update inputs" --branch update/flake flake.lock
```

- Listed paths limit the diff. Calls without paths take the whole tree, new untracked files included.
- Equal paths without `--branch` update the same pull request.
- Patches deleting a file fail with `the patch deletes <path>; deleting files is not supported`.
- Secret values in the patch receive masks before the upload.
- All options are in the [CLI reference](../reference/cli.md#pull-requests).

### After the Job

=== "UI"

    On the task, go to **Actions -> New Action**. Select **PR** as the type, choose the **Outbound Integration** and set the **Generator** to **Exec Tree**.

=== "Declarative"

    ```nix
    services.gradient.state.tasks.infra.actions = [{
      name = "tree-pr";
      type = "open_pr";
      config = {
        integration = "github-acme";
        generator = "exec_tree";
        branch_pattern = "update/{entry_point}"; # (1)!
      };
    }];
    ```

    1.  `branch_pattern`, `title_template` and `body_template` accept `{entry_point}` and `{job}`.

- Completed exec jobs of the task fire the action.
- Jobs without changes in `/work` open nothing.

## 5. Chain Evaluations

| Way | Effect |
|---|---|
| `start_evaluation` action | A new evaluation of another task in the project |
| `gradient/continue.json` output | More entry points in the running evaluation |

=== "UI"

    On the task, go to **Actions -> New Action**. Select **Start Evaluation** as the type, pick the task and the event `exec_job.completed`.

=== "Declarative"

    ```nix
    services.gradient.state.tasks.infra.actions = [{
      name = "evaluate-deploy";
      type = "start_evaluation";
      events = [ "exec_job.completed" ];
      config.task = "deploy";
    }];
    ```

- Exec jobs of the started evaluation find the starting job in `GRADIENT_PARENT_JOB`.

```sh
echo '{"attributes":["packages.x86_64-linux.model"]}' > continue.json
gradient-upload continue.json gradient/continue.json
```

- Completed jobs add the listed attributes to their evaluation. Existing entry points stay single.
- Evaluations remain `Building` until the new entry points finish.
- Attribute paths stay concrete, without `*` or `?`. Files hold at most 64 KiB.
- Files from a failed attempt count for nothing. Errors fail the evaluation with `continue.json: <error>`.
- `git_host_status_report` actions with exec events report one status per exec entry point.

## Verify Deployment

- The job log shows phase dividers.
- Job tokens, outputs, pull requests and started evaluations show on the job page.
- Pull requests appear on the Git host with the branch from `--branch` or the action.

## Next Steps

- [Run Exec Jobs](exec-jobs.md): hardware, secrets and terminals
- [Update Flake Inputs](flake-updates.md): a flake update as an exec job
- [CLI](../reference/cli.md): `gradient pr open` and `gradient build`
- [Declarative State](../reference/state.md): the `start_evaluation` and `open_pr` actions
