# Notify with Actions

Mails and web requests on evaluation and build events, e.g. a mail to the team on every failed build.

**Requirements:**

- A task, see [First Project](../get-started/first-project.md)
- [Email](../reference/configuration.md#email) configured on the server, for mail actions only

## 1. Add the Action

=== "UI"

    Open **Actions -> New Action** on the task and pick a type.

    | Type | Fields |
    |---|---|
    | Send Mail | **Recipients**, comma-separated. Optional **Subject Template** |
    | Send Web Request | **URL**. Optional **Token**, shown once after saving |

    **Send Mail** is available only with email configured on the server.

=== "Declarative"

    ```nix
    services.gradient.state.tasks.web-app.actions = [
      {
        name = "notify-ops";
        type = "send_mail";
        events = [ "evaluation.failed" "build.failed" ];
        config.recipients = [ "ops@example.com" ];
      }
      {
        name = "chat-hook";
        type = "send_web_request";
        events = [ "evaluation.completed" "evaluation.failed" ];
        config = {
          url = "https://hooks.example.com/gradient";
          token_file = "/run/secrets/chat-hook-token";
        };
      }
    ];
    ```

## 2. Pick the Events

An action without events never fires. The table is listing the events most actions need.

| Event | Trigger |
|---|---|
| `evaluation.completed` | Every build of the evaluation succeeded |
| `evaluation.failed` | The evaluation failed |
| `build.failed` | One build failed |
| `evaluation.action_required` | A pull request from a fork is waiting for maintainer approval |

The full list is in the [events reference](../reference/events.md).

## Mail

- Subject placeholders: `{event}`, `{task}`, `{project}`, `{id}`, `{status}`.
- Default subject: `[Gradient] {event}: {task}`.
- Body: the event, task, status and a link to the evaluation or build.

## Web Request

Each delivery is a `POST` with a JSON body. Receivers are reading the `content` field.

```json
{
  "event": "build.failed",
  "at": "2026-09-26T12:00:00.123Z",
  "content": { "build_id": "...", "evaluation_id": "...", "task": "...", "project": "...", "status": 4 }
}
```

| Header | Value |
|---|---|
| `X-Gradient-Event` | The event name |
| `Authorization` | `Bearer <token>`, with a token only |
| `X-Gradient-Signature` | `sha256=<HMAC-SHA256 of the body, with the token as key>`, with a token only |

Receivers can check the signature and reject requests that did not come from Gradient.

## Git Host Status Report

The **Git Host Status Report** action is posting one check per step on each commit and pull request. The [Git host guides](github.md#4-wire-the-task) are covering its setup.

| Check | State |
|---|---|
| `gradient/<task>: Approval` | Only for pull requests from forks, pending until maintainer approval |
| `gradient/<task>: Evaluation` | Pending while the evaluation is active, then success or failure |
| `gradient/<task>: Build <entry point>` | One per entry point: pending, running, then success or failure |

An evaluation started by `/gradient run <wildcard>` is reporting as `gradient/<task>: Evaluation: <wildcard>` next to the default checks.

**Test** is checking the integration's access to the repository without posting a status.

## Verify Deployment

- **Test** on the action row is sending a sample event, marked `"synthetic": true`.
- **Deliveries** on the action row is listing every request with status, duration, request and response body.

## Troubleshooting

| Symptom | Fix |
|---|---|
| **Send Mail** missing from the type list | Configure [email](../reference/configuration.md#email) on the server |
| Delivery showing `connection refused` | The URL is unreachable from the server |
| No deliveries | The action is inactive, or none of the events fired yet |

## Next Steps

- [Update Flake Inputs](flake-updates.md): pull requests that bump `flake.lock`
- [Connect GitHub](github.md): status checks with the **Git Host Status Report** action
- [Projects and Tasks](../concepts/projects-and-tasks.md): triggers and actions
