# Turn On Exec Jobs

Exec jobs for one task, with every switch on the way to a worker turned on.

**Requirements:**

- A task wildcard over the apps, for example `apps.x86_64-linux.*`, see [Evaluation Wildcards](../reference/wildcards.md)
- A worker with the `exec` capability, see [Set Up an Exec Worker](worker.md)

## 1. Check the Switches

Exec jobs need four switches. A repository alone can never start code on a worker.

| Switch | Default | Where |
|---|---|---|
| Instance | on | `services.gradient.exec.enable` |
| Project | off | Project **Settings -> Run exec jobs**, or `exec_enabled` in the state |
| Task | off | Task **Settings -> Run exec jobs**, or `exec_enabled` in the state |
| Worker | off | `services.gradient.worker.capabilities.exec` and `enable_exec` on the worker registration |

## 2. Turn On Project, Task and Worker

=== "UI"

    Open the project **Settings** and tick **Run exec jobs**. Only project admins can change the project switch.

    Open the task **Settings**, tick **Run exec jobs** and save. Members who can edit the task can change the task switch.

=== "Declarative"

    ```nix
    services.gradient.state = {
      projects.acme.exec_enabled = true;
      tasks.apps.exec_enabled = true;
      workers.gpu-box.enable_exec = true;
    };
    ```

## 3. Read Refusals

Exec apps fail with a message on the task page while the instance, project or task switch is off.

| Message | Cause |
|---|---|
| `exec jobs are disabled on this instance` | `exec.enable` is off |
| `exec jobs are disabled for this project` | The project switch is off |
| `exec jobs are disabled for this task` | The task switch is off |

A finished job can start again with `runExecJob` from the [API reference](https://petstore.swagger.io/?url=https://raw.githubusercontent.com/wavelens/gradient/main/docs/gradient-api.yaml) once every switch is on. An app refused during evaluation has no job and needs a new evaluation.

## Verify Deployment

- Start an evaluation of the task. Exec jobs appear with a status in the **Exec Jobs** section of the evaluation.
- Waiting jobs name the missing requirement, for example a device without a connected worker.

## Next Steps

- [Set Up an Exec Worker](worker.md): guest image, devices and limits
- [Declare an Exec App](apps.md): `passthru.gradient` and hardware requests
- [Declarative State](../reference/state.md): the project and task `exec_enabled` flags and `enable_exec`
- [Configuration](../reference/configuration.md): the `exec.enable` setting
