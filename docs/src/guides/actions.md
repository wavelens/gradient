# Notify with Actions

Mails, web requests and Matrix or Slack messages on evaluation and build events, e.g. a mail to the team on every failed build.

**Requirements:**

- A task, see [First Project](../get-started/first-project.md)
- [Email](../reference/configuration.md#email) configured on the server, for mail actions only
- A [Matrix access token](#matrix-access-token) or a [Slack webhook URL](#slack-webhook-url), for chat actions only

## 1. Add the Action

=== "UI"

    Open **Actions -> New Action** on the task and pick a type.

    | Type | Fields |
    |---|---|
    | Send Mail | **Recipients**, comma-separated email addresses or `team:<name>`. Optional **Subject Template** |
    | Send Web Request | **URL**. Optional **Token**, shown once after saving |
    | Send Matrix Message | **Homeserver**, **Room ID**, **Access Token** |
    | Send Slack Message | **Webhook URL** |

    **Send Mail** is available only with email configured on the server.

    A `team:<name>` recipient is a [team](../concepts/teams.md) granted with users on the project. Its verified members get the mail.

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
      {
        name = "ops-room";
        type = "send_matrix_message";
        events = [ "evaluation.failed" "build.failed" ];
        config = {
          homeserver = "https://matrix.example.org";
          room_id = "!abc123:example.org";
          access_token_file = "/run/secrets/ops-room-token";
        };
      }
      {
        name = "ops-channel";
        type = "send_slack_message";
        events = [ "evaluation.failed" ];
        config.webhook_url_file = "/run/secrets/ops-channel-webhook";
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
- Body: the [message line](#matrix-and-slack), the event, the time and a link to the evaluation log.

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

## Matrix and Slack

Both actions are posting one line per event, with a link to the evaluation log.

```text
web/app: hello-2.12.1 failed on 3f9c2ab
```

| Part | Content |
|---|---|
| `web/app` | Project and task |
| `hello-2.12.1` | Derivation, on build events only |
| `failed` | Status, taken from the event name |
| `3f9c2ab` | Short commit hash |

- Gradient is retrying rate limits (`429`) and server errors (`5xx`).
- Other HTTP errors are ending the delivery as failed, listed under **Deliveries**.
- The access token and the webhook URL are stored encrypted and never returned by the UI or the API.

### Matrix Access Token

- A dedicated bot account is the safest choice. The bot must have joined the room.
- Encrypted rooms are not supported.
- The **Room ID** is under room settings -> Advanced, in the form `!abc123:example.org`. Room aliases like `#ops:example.org` are rejected.

A password login is returning the access token of the bot account.

```sh
curl -s -X POST https://matrix.example.org/_matrix/client/v3/login \
  -d '{"type":"m.login.password","identifier":{"type":"m.id.user","user":"gradient-bot"},"password":"<password>"}' \
  | jq -r .access_token
```

### Slack Webhook URL

- A Slack app with **Incoming Webhooks** enabled is issuing one URL per channel, under **Add New Webhook to Workspace**.
- The URL is in the form `https://hooks.slack.com/services/T.../B.../...`.

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
| Matrix delivery showing `403` | The bot left the room, or the access token was revoked |
| Slack delivery showing `403` or `404` | The webhook was removed in Slack |

## Next Steps

- [Update Flake Inputs](flake-updates.md): pull requests that bump `flake.lock`
- [Connect GitHub](github.md): status checks with the **Git Host Status Report** action
- [Projects and Tasks](../concepts/projects-and-tasks.md): triggers and actions
