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

Install the configured GitHub App on your GitHub org or repository. The
installation ID is stored automatically on the matching Gradient organization.
Deliveries are authenticated via the App's webhook secret
(`GRADIENT_GITHUB_APP_WEBHOOK_SECRET_FILE`).

GitHub support appears in the UI only when the server has a GitHub App
configured. In that case, each organization can toggle *Enable GitHub App*
independently (default off) from the Integrations page.

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

## Troubleshooting

| Symptom                           | Likely cause                                                                                    |
|-----------------------------------|-------------------------------------------------------------------------------------------------|
| `401 Unauthorized` in delivery log | Secret mismatch — re-copy the secret from Gradient or rotate and reconfigure the forge.         |
| `404 Not Found`                   | Wrong organization or integration name in the URL, or `{forge}=github` (use the App webhook).   |
| `200 OK` but no evaluation runs   | No project links to this inbound integration, or the repository URL doesn't match any project.  |
| `503 Service Unavailable`         | The integration row has no secret set yet — paste or generate one on the Integrations page.     |
