# Connect GitLab

Evaluations on every push and merge request, with commit statuses back on GitLab. Two integrations connect GitLab: an **inbound** one receiving webhooks and an **outbound** one reporting status.

**Requirements:**

- A task with a repository URL pointing to GitLab (gitlab.com or self-hosted), see [First Project](../get-started/first-project.md)
- A GitLab access token with the `api` scope and at least the Developer role on the repository

## 1. Create the Integrations

=== "UI"

    Open **Integrations -> New Integration** in the project twice, once per kind.

    | Kind | Fields |
    |---|---|
    | Inbound | Name, **Git Host** GitLab and a **Webhook Secret**. The refresh button is generating a secret. The secret is shown once and must be copied. The created integration is showing the **Webhook URL**. |
    | Outbound | Name, **Git Host** GitLab, **Endpoint URL** (e.g. `https://gitlab.com`) and the **Access Token** |

=== "Declarative"

    ```nix
    services.gradient.state.integrations = {
      gitlab-in = {
        project = "acme";
        kind = "inbound";
        git_host_type = "gitlab";
        secret_file = "/run/secrets/gitlab-webhook-secret"; # (1)!
        created_by = "alice";
      };
      gitlab-out = {
        project = "acme";
        kind = "outbound";
        git_host_type = "gitlab";
        endpoint_url = "https://gitlab.com";
        access_token_file = "/run/secrets/gitlab-token";
        created_by = "alice";
      };
    };
    ```

    1.  Any random string, e.g. `openssl rand -hex 32`. The GitLab webhook is using the same value.

    The webhook URL is `https://gradient.example.com/api/v1/hooks/gitlab/acme/gitlab-in`.

## 2. Add the Webhook on GitLab

Open **Settings -> Webhooks -> Add new webhook** in the GitLab project (or group).

| Field | Value |
|---|---|
| URL | The webhook URL from step 1 |
| Secret token | The secret from step 1 |
| Trigger | **Push events**, **Tag push events**, **Comments**, **Merge request events**, **Releases events** |

A push-only webhook is never delivering merge requests or the `/gradient` comment commands.

## 3. Wire the Task

Gradient is adding a **Push (reporter)** trigger and a **Git Host Status Report** action automatically to a new task. The task must be created after the integrations. Its repository host must match exactly one inbound and one outbound integration. Other tasks need both added by hand.

- **Triggers -> New Trigger**: **Push (reporter)** and, for merge requests, **Pull Request (reporter)**, each with the inbound integration.
- **Actions -> New Action**: **Git Host Status Report** with the outbound integration.

## Verify Deployment

- A push is starting an evaluation within seconds.
- **Settings -> Webhooks -> Edit -> Recent events** is showing a `200` delivery.
- The commit on GitLab is showing Gradient's pipeline status.

## Merge Requests

| Action on GitLab | Effect |
|---|---|
| Open or update a merge request | Evaluation of the merge request's head commit |
| Comment `/gradient run` | New evaluation of the merge request |
| Comment `/gradient approve` | Release of a merge request from a fork waiting for maintainer approval |

The approval gate is a setting of the **Pull Request (reporter)** trigger: **Require maintainer approval for PRs from non-writers**. GitLab is sending no webhook for review approvals. The comment is the only way to approve.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `401` in the webhook's recent events | Secret mismatch. Enter the secret again on both sides (**Edit -> Webhook Secret** in Gradient) |
| `403 forbidden_source_ip` | GitLab's address is missing from the integration's allowed source IPs |
| `404` | Wrong project or integration name in the webhook URL, or an inbound integration without a secret |
| `200`, but no evaluation | No task trigger is using this integration, or no task repository URL is matching |
| No status on the commit | The token is lacking the `api` scope or the Developer role |

## Next Steps

- [Actions](actions.md): mail, web requests and flake update merge requests
- [Connect Gitea or Forgejo](gitea.md): the same setup for Gitea and Forgejo
