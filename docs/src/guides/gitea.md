# Connect Gitea or Forgejo

Evaluations on every push and pull request, with commit statuses back on the Git host. Two integrations connect the Git host: an **inbound** one receiving webhooks and an **outbound** one reporting status.

**Requirements:**

- A task with a repository URL on the Gitea or Forgejo instance, see [First Project](../get-started/first-project.md)
- A Git host access token with repository write access

## 1. Create the Integrations

=== "UI"

    Open **Integrations -> New Integration** in the project twice, once per kind.

    | Kind | Fields |
    |---|---|
    | Inbound | Name, **Git Host** Gitea or Forgejo and a **Webhook Secret**. The refresh button can generate a secret. The secret is shown once and must be copied. The created integration will show the **Webhook URL**. |
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
    2.  Any random string, e.g. `openssl rand -hex 32`. The Git host webhook must use the same value.

    The webhook URL is `https://gradient.example.com/api/v1/hooks/gitea/acme/gitea-in` (`forgejo` instead of `gitea` for Forgejo).

## 2. Add the Webhook on the Git Host

Open **Settings -> Webhooks -> Add Webhook** in the repository (or organization) on the Git host.

| Field | Value |
|---|---|
| Target URL | The webhook URL from step 1 |
| HTTP method, content type | `POST`, `application/json` |
| Secret | The secret from step 1 |
| Trigger on | Custom events: **Push**, **Pull Request**, **Issue Comment**, **Pull Request Comment**, **Pull Request Review**, **Release** |

A push-only webhook is never delivering pull requests or the `/gradient` comment commands.

## 3. Wire the Task

Gradient will add a **Push (reporter)** trigger and a **Git Host Status Report** action automatically to a new task. The task must be created after the integrations. Its repository host must match exactly one inbound and one outbound integration. Other tasks need both added by hand.

- **Triggers -> New Trigger**: **Push (reporter)** and, for pull requests, **Pull Request (reporter)**, each with the inbound integration.
- **Actions -> New Action**: **Git Host Status Report** with the outbound integration.

## Verify Deployment

- A push will start an evaluation within seconds.
- The Git host's webhook page will show a `200` delivery.
- The commit on the Git host will show Gradient's status.

## Pull Requests

| Action on the Git host | Effect |
|---|---|
| Open or update a pull request | Evaluation of the pull request's head commit |
| Comment `/gradient run` | New evaluation of the pull request |
| Comment `/gradient approve` or approve the review | Release of a pull request from a fork waiting for maintainer approval |

The approval check is a setting of the **Pull Request (reporter)** trigger: **Require maintainer approval for PRs from non-writers**.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `401` in the Git host's delivery log | Secret mismatch. Enter the secret again on both sides (**Edit -> Webhook Secret** in Gradient) |
| `403 forbidden_source_ip` | The Git host's address is missing from the integration's allowed source IPs |
| `404` | Wrong project or integration name in the webhook URL, or an inbound integration without a secret |
| `200`, but no evaluation | No task trigger is using this integration, or no task repository URL is matching |

## Next Steps

- [Actions](actions.md): mail, web requests and flake update pull requests
- [Connect GitLab](gitlab.md): the same setup for GitLab
