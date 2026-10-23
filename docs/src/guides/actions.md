<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# Notify with Actions

Actions send notifications when evaluation or build events occur. Examples: a mail to the team on every failed build, a web request on each successful build, or a Matrix or Slack message.

**Requirements:**

- A task, see [First Project](../get-started/first-project.md)
- [Email](../reference/configuration.md#email) configured on the server, needed only for mail actions
- A [Matrix access token](#matrix-access-token) or a [Slack webhook URL](#slack-webhook-url), needed only for chat actions

## 1. Add the Action

=== "UI"

    On the task, go to **Actions -> New Action** and select a type.

    | Type | Fields |
    |---|---|
    | Mail | **Recipients**, a comma-separated list of email addresses or `team:<name>`. An optional **Subject Template** |
    | Webhook | **URL**. An optional **Token**, displayed one time after saving |
    | Matrix | **Homeserver**, **Room ID**, **Access Token** |
    | Slack | **Webhook URL** |

    **Mail** works only after email has been configured on the server.

    A `team:<name>` recipient names a [team](../concepts/teams.md) granted with users on the project. Mail goes to its verified members.

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

An action without events can never fire. Most actions need only a few of them, listed here.

| Event | Trigger |
|---|---|
| `evaluation.completed` | All builds in the evaluation passed |
| `evaluation.failed` | The evaluation failed |
| `build.failed` | A single build failed |
| `evaluation.action_required` | A fork pull request await maintainer approval |

The [events reference](../reference/events.md) list every available event.

## Mail

- Subject placeholders: `{event}`, `{task}`, `{project}`, `{id}`, `{status}`.
- Default subject: `[Gradient] {event}: {task}`.
- Body: the [message line](#matrix-and-slack), the event, the time and a link to the evaluation log.

## Web Request

Every delivery arrives as a `POST` request with a JSON body. The `content` field holds the event data.

```json
{
  "event": "build.failed",
  "at": "2026-09-26T12:00:00.123Z",
  "content": { "build_id": "...", "evaluation_id": "...", "task": "...", "project": "...", "status": 4 }
}
```

| Header | Value |
|---|---|
| `X-Gradient-Event` | Name of the event |
| `Authorization` | `Bearer <token>` with the token only |
| `X-Gradient-Signature` | `sha256=<HMAC-SHA256 of the body, with the token as key>` with the token only |

A receiver can verify the signature to reject requests not sent by Gradient.

## Matrix and Slack

Each event becomes a single message line with a link to the evaluation log.

```text
web/app: hello-2.12.1 failed on 3f9c2ab
```

| Part | Content |
|---|---|
| `web/app` | Project and task |
| `hello-2.12.1` | Derivation, on build events only |
| `failed` | Status, taken from the event name |
| `3f9c2ab` | Short commit hash |

- Gradient retries rate limits (`429`) and server errors (`5xx`).
- Any other HTTP error ends the delivery as failed. Failed deliveries show up under **Deliveries**.
- The access token and the webhook URL sit in storage in encrypted form. The UI and the API never send them back.

### Matrix Access Token

- Use a dedicated bot account. The bot has to have joined the room.
- Encrypted rooms do not work.
- Find the **Room ID** under room settings -> Advanced, in the form `!abc123:example.org`. Rooms of version 12 and later have IDs without the server part, like `!abc123`. Room aliases like `#ops:example.org` get rejected.

A password login gives the access token of the bot account.

```sh
curl -s -X POST https://matrix.example.org/_matrix/client/v3/login \
  -d '{"type":"m.login.password","identifier":{"type":"m.id.user","user":"gradient-bot"},"password":"<password>"}' \
  | jq -r .access_token
```

### Slack Webhook URL

- Slack apps with **Incoming Webhooks** enabled issue one URL per channel, under **Add New Webhook to Workspace**.
- The URL looks like `https://hooks.slack.com/services/T.../B.../...`.

## Git Status

The **Git Status** action can post one check per step to each commit and pull request. Setup instructions appear in the [Git host guides](github.md#4-wire-the-task).

| Check | State |
|---|---|
| `gradient/<task>: Approval` | Fork pull requests only. Stays pending until a maintainer approves |
| `gradient/<task>: Evaluation` | Pending while the evaluation runs, then reports success or failure. A failure marks any entry point that could not be evaluated, while the remaining builds keep going |
| `gradient/<task>: Build <entry point>` | One check per entry point. Moves from pending to running, then to success or failure. An entry point that failed to evaluate shows failure with no build |

An evaluation started with `/gradient run <wildcard>` reports as `gradient/<task>: Evaluation: <wildcard>` in addition to the default checks.

**Test** verifies the repository access of the integration without posting a status.

## Verify Deployment

- Each action row contains a **Test** button. A click sends a sample event to the receiver with `"synthetic": true` set.
- The **Deliveries** view on each action row shows all requests with their status, duration, request body and response body.

## Troubleshooting

| Symptom | Fix |
|---|---|
| **Mail** absent from the type list | Set up [email](../reference/configuration.md#email) on the server |
| Delivery fails with `connection refused` | The server cannot reach the URL |
| Deliveries never happen | The action was deactivated, or none of the events fired |
| Matrix delivery fails with `403` | The bot left the room, or the access token got revoked |
| Slack delivery fails with `403` or `404` | The webhook was deleted in Slack |

## Next Steps

- [Update Flake Inputs](flake-updates.md): automate pull requests that refresh `flake.lock`
- [Connect GitHub](github.md): report commit statuses through the **Git Status** action
- [Projects and Tasks](../concepts/projects-and-tasks.md): how triggers and actions work together
