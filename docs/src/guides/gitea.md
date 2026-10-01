# Connect Gitea or Forgejo

Evaluations on every push and pull request, with commit statuses back on the Git host, through two integrations: an **inbound** one that receives webhooks and an **outbound** one that reports status.

**Requirements:**

- A task whose repository URL points to the Gitea or Forgejo instance, see [First Project](../get-started/first-project.md)
- An access token on the Git host with write access to the repository

## 1. Create the Integrations

=== "UI"

    In the project, **Integrations -> New Integration** twice:

    | Kind | Fields |
    |---|---|
    | Inbound | Name, **Git Host** Gitea or Forgejo and a **Webhook Secret** (the refresh button generates one; copy the value, the secret is shown once). The created integration shows the **Webhook URL**. |
    | Outbound | Name, **Git Host**, **Endpoint URL** (e.g. `https://gitea.example.com`) and the **Access Token** |

=== "Declarative"

    ```nix
    services.gradient.state.integrations = {
      gitea-in = {
        project = "acme";
        kind = "inbound";
        git_host_type = "gitea"; # (1)!
        secret_file = "/run/secrets/gitea-webhook-secret"; # (2)!
        created_by = "alice";
      };
      gitea-out = {
        project = "acme";
        kind = "outbound";
        git_host_type = "gitea";
        endpoint_url = "https://gitea.example.com";
        access_token_file = "/run/secrets/gitea-token";
        created_by = "alice";
      };
    };
    ```

    1.  `forgejo` for Forgejo.
    2.  Any random string, e.g. `openssl rand -hex 32`; the Git host webhook uses the same value.

    The webhook URL is `https://gradient.example.com/api/v1/hooks/gitea/acme/gitea-in` (`forgejo` instead of `gitea` for Forgejo).

## 2. Add the Webhook on the Git Host

In the repository (or organization) on the Git host, **Settings -> Webhooks -> Add Webhook**:

| Field | Value |
|---|---|
| Target URL | The webhook URL from step 1 |
| HTTP method, content type | `POST`, `application/json` |
| Secret | The secret from step 1 |
| Trigger on | Custom events: **Push**, **Pull Request**, **Issue Comment**, **Pull Request Comment**, **Pull Request Review**, **Release** |

A push-only webhook never delivers pull requests or the `/gradient` comment commands.

## 3. Wire the Task

A task created after the integrations, whose repository host matches exactly one inbound and one outbound integration, gets a **Push (reporter)** trigger and a **Git Host Status Report** action automatically. Otherwise, on the task:

- **Triggers -> New Trigger**: **Push (reporter)** and, for pull requests, **Pull Request (reporter)**, each with the inbound integration.
- **Actions -> New Action**: **Git Host Status Report** with the outbound integration.

## Verify Deployment

- A push starts an evaluation within seconds; the Git host's webhook page shows a `200` delivery.
- The commit on the Git host shows Gradient's status.

## Pull Requests

| Action on the Git host | Effect |
|---|---|
| Open or update a pull request | Evaluates the pull request's head commit |
| Comment `/gradient run` | Starts an evaluation of the pull request |
| Comment `/gradient approve` or approve the review | Releases a pull request from a fork waiting for maintainer approval |

The approval gate is a setting of the **Pull Request (reporter)** trigger: **Require maintainer approval for PRs from non-writers**.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `401` in the Git host's delivery log | Secret mismatch; enter the secret again on both sides (**Edit -> Webhook Secret** in Gradient) |
| `403 forbidden_source_ip` | The Git host's address is missing from the integration's allowed source IPs |
| `404` | Wrong project or integration name in the webhook URL, or the inbound integration has no secret |
| `200`, but no evaluation | No task trigger uses this integration, or no task repository URL matches |

## Next Steps

- [Actions](actions.md): mail, web requests and flake update pull requests
- [Connect GitLab](gitlab.md): the same setup for GitLab
