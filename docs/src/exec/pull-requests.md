# Open Pull Requests

Pull requests from the changes of an exec job, opened from inside the program or after the job.

**Requirements:**

- An exec app, see [Declare an Exec App](apps.md)
- An outbound Git host integration, see [Connect GitHub](../guides/github.md), [Connect Gitea or Forgejo](../guides/gitea.md) or [Connect GitLab](../guides/gitlab.md)

## 1. Call Gradient from the Job { #job-token }

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
- Changed trees reach the workers through `gradient build`, see [Build Before Pushing](../guides/build-before-push.md). Job pages link each such evaluation.

## 2. Pick a Way

| Way | Pick for |
|---|---|
| `gradient pr open` in the program | Many pull requests per job, each with chosen paths |
| `exec_tree` PR action | One pull request with every change, without a CLI call |

Both ways need an `open_pr` action naming the integration. Tasks without one answer `the task has no open_pr action naming a Git host integration`.

## 3. Open from the Program

```sh
gradient pr open --title "flake: update inputs" --branch update/flake flake.lock
```

- Listed paths limit the diff. Calls without paths take the whole tree, new untracked files included.
- Equal paths without `--branch` update the same pull request.
- Patches deleting a file fail with `the patch deletes <path>; deleting files is not supported`.
- Secret values in the patch receive masks before the upload.
- All options are in the [CLI reference](../reference/cli.md#pull-requests).

## 4. Open after the Job

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

## Verify Deployment

- Job tokens and pull requests show on the job page.
- Pull requests appear on the Git host with the branch from `--branch` or the action.

## Next Steps

- [Update Flake Inputs](../guides/flake-updates.md): a flake update as an exec job
- [CLI](../reference/cli.md): `gradient pr open` and `gradient build`
- [Declarative State](../reference/state.md): the `open_pr` action
