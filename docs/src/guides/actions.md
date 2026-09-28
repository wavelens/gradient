# Notify with Actions

Mails and web requests on evaluation and build events, e.g. a mail to the team on every failed build.

**Requirements:**

- A task, see [First project](../get-started/first-project.md)
- For mail: [email](../configuration.md#email) configured on the server

## 1. Add the Action

=== "UI"

    On the task, **Actions -> New Action**, then pick a type:

    | Type | Fields |
    |---|---|
    | Send Mail | **Recipients**, comma-separated; optional **Subject Template** |
    | Send Web Request | **URL**; optional **Token**, shown once after saving |

    **Send Mail** only shows when email is configured on the server.

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

An action without events never fires. The events most actions need:

| Event | Fires when |
|---|---|
| `evaluation.completed` | Every build of the evaluation succeeded |
| `evaluation.failed` | The evaluation failed |
| `build.failed` | One build failed |
| `evaluation.action_required` | A pull request from a fork waits for maintainer approval |

The full list is in the [events reference](../usage/events.md).

## Mail

- Subject placeholders: `{event}`, `{task}`, `{project}`, `{id}`, `{status}`.
- Default subject: `[Gradient] {event}: {task}`.
- The body holds the event, task, status and a link to the evaluation or build.

## Web Request

A `POST` with a JSON body; receivers read `content`:

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
| `X-Gradient-Signature` | `sha256=<HMAC-SHA256 of the body, keyed by the token>`, with a token only |

The signature lets the receiver reject requests that did not come from Gradient.

## Verify Deployment

- **Test** on the action row sends a sample event, marked `"synthetic": true`.
- **Deliveries** on the action row lists every request with status, duration, request and response body.

## Troubleshooting

| Symptom | Fix |
|---|---|
| **Send Mail** missing from the type list | Configure [email](../configuration.md#email) on the server |
| Delivery shows `connection refused` | The URL is unreachable from the server |
| No deliveries | The action is inactive, or none of the events fired yet |

## Next Steps

- [Update Flake Inputs](flake-updates.md): pull requests that bump `flake.lock`
- [Connect GitHub](forge-github.md): status checks with the **Forge Status Report** action
- [Projects and Tasks](../concepts/projects-and-tasks.md): triggers and actions
