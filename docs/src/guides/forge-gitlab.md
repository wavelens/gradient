# Connect GitLab

Evaluations on every push and merge request, with commit statuses back on GitLab, through two integrations: an **inbound** one that receives webhooks and an **outbound** one that reports status.

**Requirements:**

- A task whose repository URL points to GitLab (gitlab.com or self-hosted), see [First project](../get-started/first-project.md)
- A GitLab access token with the `api` scope and at least the Developer role on the repository

## 1. Create the Integrations

=== "UI"

    In the project, **Integrations -> New Integration** twice:

    | Kind | Fields |
    |---|---|
    | Inbound | Name, forge GitLab. Gradient generates the **Secret** and shows the **Webhook URL**; copy both, the secret is shown once. |
    | Outbound | Name, forge GitLab, **Endpoint URL** (e.g. `https://gitlab.com`) and the **Access Token** |

=== "Declarative"

    ```nix
    services.gradient.state.integrations = {
      gitlab-in = {
        project = "acme";
        kind = "inbound";
        forge_type = "gitlab";
        secret_file = "/run/secrets/gitlab-webhook-secret"; # (1)!
        created_by = "alice";
      };
      gitlab-out = {
        project = "acme";
        kind = "outbound";
        forge_type = "gitlab";
        endpoint_url = "https://gitlab.com";
        access_token_file = "/run/secrets/gitlab-token";
        created_by = "alice";
      };
    };
    ```

    1.  Any random string, e.g. `openssl rand -hex 32`; the GitLab webhook uses the same value.

    The webhook URL is `https://gradient.example.com/api/v1/hooks/gitlab/acme/gitlab-in`.

## 2. Add the Webhook on GitLab

In the GitLab project (or group), **Settings -> Webhooks -> Add new webhook**:

| Field | Value |
|---|---|
| URL | The webhook URL from step 1 |
| Secret token | The secret from step 1 |
| Trigger | **Push events**, **Tag push events**, **Comments**, **Merge request events**, **Releases events** |

A push-only webhook never delivers merge requests or the `/gradient` comment commands.

## 3. Wire the Task

A task created after the integrations, whose repository host matches exactly one inbound and one outbound integration, gets a **Push (reporter)** trigger and a **Forge Status Report** action automatically. Otherwise, on the task:

- **Triggers -> New Trigger**: **Push (reporter)** and, for merge requests, **Pull Request (reporter)**, each with the inbound integration.
- **Actions -> New Action**: **Forge Status Report** with the outbound integration.

## Verify Deployment

- A push starts an evaluation within seconds; **Settings -> Webhooks -> Edit -> Recent events** shows a `200` delivery.
- The commit on GitLab shows Gradient's pipeline status.

## Merge Requests

| Action on GitLab | Effect |
|---|---|
| Open or update a merge request | Evaluates the merge request's head commit |
| Comment `/gradient run` | Starts an evaluation of the merge request |
| Comment `/gradient approve` | Releases a merge request from a fork waiting for maintainer approval |

The approval gate is a task setting: **Require maintainer approval for PRs from non-writers**. GitLab sends no webhook for review approvals, so the comment is the only way to approve.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `401` in the webhook's recent events | Secret mismatch; copy the secret again or rotate the integration |
| `403 forbidden_source_ip` | GitLab's address is missing from the integration's allowed source IPs |
| `404` | Wrong project or integration name in the webhook URL |
| `503` | The inbound integration has no secret yet |
| `200`, but no evaluation | No task trigger uses this integration, or no task repository URL matches |
| No status on the commit | The token lacks the `api` scope or the Developer role |

## Next Steps

- [Actions](../usage/actions.md): mail, web requests and flake update merge requests
- [Connect Gitea or Forgejo](forge-gitea.md): the same setup for Gitea and Forgejo
