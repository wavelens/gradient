# Chain Evaluations

Completed exec jobs start evaluations and entry points.

**Requirements:**

- An exec app, see [Declare an Exec App](apps.md)

## 1. Pick a Way

| Way | Effect |
|---|---|
| `start_evaluation` action | A new evaluation of another task in the project |
| `gradient/continue.json` output | More entry points in the running evaluation |

## 2. Start Another Task

=== "UI"

Go to **Actions -> New Action** in the task. Select **Start Evaluation** as the type and choose the task and event `exec_job.completed`.

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

## 3. Add Entry Points

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

- Started evaluations appear on the job page.
- New entry points appear in the evaluation for the job.

## Next Steps

- [Report Phases and Upload Outputs](phases-and-outputs.md): `gradient-upload`
- [Declarative State](../reference/state.md): the `start_evaluation` action
- [Events and Webhooks](../reference/events.md): `exec_job.*` events
