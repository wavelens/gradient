# Connect Gitea or Forgejo

Evaluations on every push and pull request, with commit statuses back on the forge, through two integrations: an **inbound** one that receives webhooks and an **outbound** one that reports status.

**Requirements:**

- A task whose repository URL points to the Gitea or Forgejo instance, see [First project](../get-started/first-project.md)
- An access token on the forge with write access to the repository

## 1. Create the Integrations

=== "UI"

    In the project, **Integrations -> New Integration** twice:

    | Kind | Fields |
    |---|---|
    | Inbound | Name, forge Gitea or Forgejo. Gradient generates the **Secret** and shows the **Webhook URL**; copy both, the secret is shown once. |
    | Outbound | Name, forge, **Endpoint URL** (e.g. `https://gitea.example.com`) and the **Access Token** |

=== "Declarative"

    ```nix
    services.gradient.state.integrations = {
      gitea-in = {
        project = "acme";
        kind = "inbound";
        forge_type = "gitea"; # (1)!
        secret_file = "/run/secrets/gitea-webhook-secret"; # (2)!
        created_by = "alice";
      };
      gitea-out = {
        project = "acme";
        kind = "outbound";
        forge_type = "gitea";
        endpoint_url = "https://gitea.example.com";
        access_token_file = "/run/secrets/gitea-token";
        created_by = "alice";
      };
    };
    ```

    1.  `forgejo` for Forgejo.
    2.  Any random string, e.g. `openssl rand -hex 32`; the forge webhook uses the same value.

    The webhook URL is `https://gradient.example.com/api/v1/hooks/gitea/acme/gitea-in` (`forgejo` instead of `gitea` for Forgejo).

## 2. Add the Webhook on the Forge

In the repository (or organization) on the forge, **Settings -> Webhooks -> Add Webhook**:

| Field | Value |
|---|---|
| Target URL | The webhook URL from step 1 |
| HTTP method, content type | `POST`, `application/json` |
| Secret | The secret from step 1 |
| Trigger on | Custom events: **Push**, **Pull Request**, **Issue Comment**, **Pull Request Comment**, **Pull Request Review**, **Release** |

A push-only webhook never delivers pull requests or the `/gradient` comment commands.

## 3. Wire the Task

A task created after the integrations, whose repository host matches exactly one inbound and one outbound integration, gets a **Push (reporter)** trigger and a **Forge Status Report** action automatically. Otherwise, on the task:

- **Triggers -> New Trigger**: **Push (reporter)** and, for pull requests, **Pull Request (reporter)**, each with the inbound integration.
- **Actions -> New Action**: **Forge Status Report** with the outbound integration.

## Verify Deployment

- A push starts an evaluation within seconds; the forge's webhook page shows a `200` delivery.
- The commit on the forge shows Gradient's status.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `401` in the forge's delivery log | Secret mismatch; copy the secret again or rotate the integration |
| `403 forbidden_source_ip` | The forge's address is missing from the integration's allowed source IPs |
| `404` | Wrong project or integration name in the webhook URL |
| `503` | The inbound integration has no secret yet |
| `200`, but no evaluation | No task trigger uses this integration, or no task repository URL matches |

## Next Steps

- [Actions](../usage/actions.md): mail, web requests and flake update pull requests
