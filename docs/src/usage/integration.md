# Forge Integration

Gradient connects to a Git forge through named **integrations** owned by each
organization. An integration is either **inbound** (the forge pushes events to
Gradient) or **outbound** (Gradient reports build status back). Each project
links to at most one inbound and one outbound integration.

Supported forges: **Gitea**, **Forgejo**, **GitLab**, and **GitHub** (via the
GitHub App).

## 1. Create an integration

In the Gradient UI:

1. Open your organization → **Integrations**.
2. Click **New integration**.
3. Fill in:
    - **Name** — short slug (`production`, `gitea-status`, ...); must be unique
      within the organization and kind.
    - **Kind** — *Inbound* (Gradient receives webhooks) or *Outbound*
      (Gradient calls the forge API).
    - **Forge type** — Gitea / Forgejo / GitLab / GitHub.

Then, depending on the kind:

- **Inbound**: Gradient generates an HMAC-SHA256 secret (client-side via
  `crypto.getRandomValues`, displayed once). Copy both the secret and the
  forge-specific webhook URL — the page shows a URL selector so you can switch
  between `/hooks/gitea/...`, `/hooks/forgejo/...`, and `/hooks/gitlab/...`
  from a single inbound row.
- **Outbound**: enter the forge base URL (e.g. `https://gitea.example.com`) and
  an API token with permission to post commit statuses. For GitHub outbound,
  leave the token blank — credentials come from the server-configured GitHub
  App.

Secrets and tokens are stored encrypted with the server's crypt key; the API
never returns them again, only a boolean indicating their presence.

## 2. Link a project

Open the project's **Settings** page. Two dropdowns list all inbound and
outbound integrations for the organization; pick the ones this project should
use (or leave at *None*).

## 3. Configure the forge webhook

### Gitea / Forgejo

Repository or organization webhook → `POST` → `application/json`, URL from
Gradient, secret = the integration's secret, trigger on *Push events*.
Signatures are verified via `X-Gitea-Signature` (HMAC-SHA256 over the raw body).

### GitLab

Project or group webhook → URL from Gradient, **Secret token** = the
integration's secret, trigger = *Push events*. Gradient compares the
`X-Gitlab-Token` header against the stored secret.

### GitHub App

The GitHub integration uses a single GitHub App registered against your
Gradient server. There are three roles to consider:

1. **Server operator** — once per Gradient instance, register the App and put
   its credentials into the server's config. See the
   [GitHub App setup](../development/github-app-setup.md) operator doc.
2. **Organization admin** — once the server has the App configured, enable the
   *GitHub App* toggle on the Integrations page for each organization that
   wants to use it.
3. **GitHub repository owner** — install the App on a GitHub user or
   organization account. Gradient stores the installation ID automatically
   when the `installation` webhook fires; deliveries from the matching
   repositories then route to the corresponding Gradient organization.

Webhook deliveries are signed with the App's webhook secret and verified
server-side. Build statuses are reported back via the App's installation token.

## Rotating or deleting an integration

From the Integrations page:

- **Edit** — update name/URL, or paste a new secret/token to replace the
  existing one. Submitting an empty string for a secret/token **clears** it.
- **Delete** — removes the row. Any project linked to it has the link cleared
  (`ON DELETE SET NULL`).

## Inbound URL reference

All inbound webhook URLs have the form:

```
{serveUrl}/api/v1/hooks/{forge}/{organization}/{integration_name}
```

where `{forge}` is `gitea`, `forgejo`, or `gitlab`. GitHub deliveries go
through the App webhook at `/api/v1/hooks/github` and are not per-integration.

A single inbound integration can serve all three Gitea/Forgejo/GitLab forges
simultaneously — the signature scheme is selected by the `{forge}` path
segment.

### Webhook response body

Both `POST /api/v1/hooks/{forge}/{org}/{integration_name}` and `POST /api/v1/hooks/github`
return the standard envelope with a `WebhookResponse` payload describing what happened:

```json
{
  "error": false,
  "message": {
    "event": "push",
    "repository_urls": ["https://github.com/acme/widgets.git"],
    "projects_scanned": 2,
    "queued": [
      {
        "project_id": "...",
        "project_name": "widgets",
        "organization": "acme",
        "evaluation_id": "..."
      }
    ],
    "skipped": [
      {
        "project_id": "...",
        "project_name": "widgets-staging",
        "organization": "acme",
        "reason": "already_in_progress"
      }
    ]
  }
}
```

The `reason` field for skipped projects is one of:

- `already_in_progress` — an evaluation for the same revision is already queued or running
- `no_previous_evaluation` — the project has not yet been bootstrapped
- `db_error` — a per-project persistence failure (the request as a whole still succeeded)

Non-push GitHub App events (`ping`, `installation`, `installation_repositories`, unknown)
return the same envelope with `event` set accordingly and empty `queued` / `skipped` arrays.

## Troubleshooting

| Symptom                           | Likely cause                                                                                    |
|-----------------------------------|-------------------------------------------------------------------------------------------------|
| `401 Unauthorized` in delivery log | Secret mismatch — re-copy the secret from Gradient or rotate and reconfigure the forge.         |
| `404 Not Found`                   | Wrong organization or integration name in the URL, or `{forge}=github` (use the App webhook).   |
| `200 OK` but no evaluation runs   | No project links to this inbound integration, or the repository URL doesn't match any project.  |
| `503 Service Unavailable`         | The integration row has no secret set yet — paste or generate one on the Integrations page.     |
