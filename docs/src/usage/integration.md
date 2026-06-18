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
    - **Name** - short slug (`production`, `gitea-status`, ...); must be unique
      within the organization and kind.
    - **Kind** - *Inbound* (Gradient receives webhooks) or *Outbound*
      (Gradient calls the forge API).
    - **Forge type** - Gitea / Forgejo / GitLab. (GitHub does not appear here:
      its inbound + outbound rows are server-managed; see *GitHub App* below.)

Then, depending on the kind:

- **Inbound**: Gradient generates an HMAC-SHA256 secret (client-side via
  `crypto.getRandomValues`, displayed once). Copy both the secret and the
  forge-specific webhook URL - the page shows a URL selector so you can switch
  between `/hooks/gitea/...`, `/hooks/forgejo/...`, and `/hooks/gitlab/...`
  from a single inbound row.
- **Outbound**: enter the forge base URL (e.g. `https://gitea.example.com`) and
  an API token with permission to post commit statuses.

Secrets and tokens are stored encrypted with the server's crypt key; the API
never returns them again, only a boolean indicating their presence.

### GitHub App rows

When the GitHub App is installed on an organization, Gradient automatically
creates two `forge_type=github` integration rows for it: one *inbound* and one
*outbound*, both named `github`. They appear in the org Integrations list as
**Server-managed** and cannot be edited or deleted from the UI - their
credentials come from the server's App config and the org's installation id.

The integration name `github` is reserved system-wide for these rows; user-created
integrations cannot use it.

Reference these rows from project triggers (inbound) and project
`outbound_integration` (outbound) the same way you'd reference any other
integration.

## 2. Report build status

To report build status back to a forge, create a `forge_status_report` action on the project (see [Actions](./actions.md)).

When a project is created and its repository URL unambiguously matches one of the organization's integrations, Gradient auto-attaches the wiring: a push trigger for the matching inbound integration and a `forge_status_report` action for the matching outbound integration (at most one of each). Ambiguous matches are left for manual setup.

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

1. **Server operator** - once per Gradient instance, register the App and put
   its credentials into the server's config. See the
   [GitHub App setup](../development/github-app-setup.md) operator doc.
2. **Organization admin** - once the server has the App configured, install the
   App on the organization's GitHub account.
3. **GitHub repository owner** - installing the App fires the `installation`
   webhook, which carries the list of granted repositories. Gradient stores the
   installation id on every organization owning a project whose repository URL
   resolves to one of those repositories, and seeds the `github-app` inbound +
   outbound integration rows. Matching is purely on the repository URL: the
   organization name and the Gradient project name need not match GitHub, and
   the flake shorthand (`github:owner/repo`) is recognized alongside the https
   and SSH clone URLs. Subsequent push / pull-request deliveries route to the
   corresponding Gradient organization, and projects can link to the outbound
   row to enable status reporting.

Webhook deliveries are signed with the App's webhook secret and verified
server-side. Build statuses are reported back via the App's installation token.

## Rotating or deleting an integration

From the Integrations page:

- **Edit** - update name/URL, or paste a new secret/token to replace the
  existing one. Submitting an empty string for a secret/token **clears** it.
- **Delete** - removes the row. Any project linked to it has the link cleared
  (`ON DELETE SET NULL`).

## Inbound URL reference

All inbound webhook URLs have the form:

```text
{serveUrl}/api/v1/hooks/{forge}/{organization}/{integration_name}
```

where `{forge}` is `gitea`, `forgejo`, or `gitlab`. GitHub deliveries go
through the App webhook at `/api/v1/hooks/github` and are not per-integration.

A single inbound integration can serve all three Gitea/Forgejo/GitLab forges
simultaneously - the signature scheme is selected by the `{forge}` path
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

- `already_in_progress` - an evaluation for the same revision is already queued or running
- `no_previous_evaluation` - the project has not yet been bootstrapped
- `db_error` - a per-project persistence failure (the request as a whole still succeeded)

Non-push GitHub App events (`ping`, `installation`, `installation_repositories`, unknown)
return the same envelope with `event` set accordingly and empty `queued` / `skipped` arrays.

## Source IP restrictions

Each inbound integration can carry a CIDR allowlist (`allowed_ips`). When set,
deliveries whose source IP does not match are rejected with
`403 forbidden_source_ip` after signature verification succeeds. An empty or
omitted list allows any source.

For the per-forge route (`POST /hooks/{forge}/{org}/{integration_name}`) the
check applies to the resolved client IP. For the GitHub App route
(`POST /hooks/github`), the check is applied per-installation: integrations
whose allowlist rejects the source IP are simply skipped, while integrations
whose list matches (or is empty) are dispatched as usual.

Forge IP ranges to allowlist:

- **GitHub**: published at `https://api.github.com/meta` (the `hooks` array).
- **GitLab.com**: published at <https://docs.gitlab.com/ee/user/gitlab_com/#ip-range>.
- **Gitea / Forgejo**: typically self-hosted; allowlist your own forge's egress IPs.

The source IP is resolved from the connection peer with `X-Forwarded-For`
honored only when the peer is in `GRADIENT_NETWORK_TRUSTED_PROXIES`.

## Troubleshooting

| Symptom                           | Likely cause                                                                                    |
|-----------------------------------|-------------------------------------------------------------------------------------------------|
| `401 Unauthorized` in delivery log | Secret mismatch - re-copy the secret from Gradient or rotate and reconfigure the forge.         |
| `403 forbidden_source_ip`          | The forge's egress IP isn't in the integration's `allowed_ips` list. Add it or clear the list.   |
| `404 Not Found`                   | Wrong organization or integration name in the URL, or `{forge}=github` (use the App webhook).   |
| `200 OK` but no evaluation runs   | No project links to this inbound integration, or the repository URL doesn't match any project.  |
| `503 Service Unavailable`         | The integration row has no secret set yet - paste or generate one on the Integrations page.     |
