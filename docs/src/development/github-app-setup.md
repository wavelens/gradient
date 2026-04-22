# Setting up the GitHub App

Gradient's GitHub integration uses a single GitHub App that is registered once
per Gradient instance. This guide walks an operator through the manifest flow
that registers the App and produces the credentials Gradient needs.

## Prerequisites

- A Gradient account with the `superuser` flag.
- Admin rights on the GitHub user or organization that will own the App.
- An externally reachable HTTPS URL for your Gradient server (`GRADIENT_SERVE_URL`).

## Manifest flow (recommended)

1. Sign in to Gradient as a superuser.
2. Navigate to `/admin/github-app`.
3. (Enterprise only) Enter the GitHub Enterprise host, e.g. `ghe.example.com`.
   Leave blank for github.com.
4. Click **Create on GitHub**. Your browser is redirected to GitHub with the
   manifest pre-filled.
5. On GitHub, review the App name, permissions, and events; you can edit the
   name. Click **Create GitHub App**.
6. GitHub redirects you back to `/admin/github-app?ready=1`.
7. Copy each credential block:
   - **App ID** — set as `GRADIENT_GITHUB_APP_ID`.
   - **Private key (PEM)** — write to a file and set the path as
     `GRADIENT_GITHUB_APP_PRIVATE_KEY_FILE` (e.g. `/run/secrets/gradient-github-app.pem`).
   - **Webhook secret** — write to a file and set the path as
     `GRADIENT_GITHUB_APP_WEBHOOK_SECRET_FILE`.
8. Restart Gradient. The GitHub App toggle now appears on org Integration pages.

The credentials are returned **once**. If you navigate away before copying,
re-run the manifest flow to generate a fresh App.

## Manual flow

If the manifest flow doesn't suit your environment, register the App by hand
following [GitHub's docs](https://docs.github.com/en/apps/creating-github-apps/registering-a-github-app/registering-a-github-app).
Match the values Gradient expects:

| Setting | Value |
|---|---|
| Webhook URL | `{serveUrl}/api/v1/hooks/github` |
| Setup URL | `{serveUrl}/admin/github-app` (optional) |
| Permissions | `metadata: read`, `contents: read`, `pull_requests: read`, `statuses: write`, `checks: write` |
| Events | `push`, `pull_request`, `installation`, `installation_repositories` |

Then download the private key, generate a webhook secret, and configure the
env vars as below.

## Required configuration

| Env var | Nix option | Description |
|---|---|---|
| `GRADIENT_GITHUB_APP_ID` | `services.gradient.githubApp.appId` | Numeric App ID |
| `GRADIENT_GITHUB_APP_PRIVATE_KEY_FILE` | `services.gradient.githubApp.privateKeyFile` | Path to the PEM file |
| `GRADIENT_GITHUB_APP_WEBHOOK_SECRET_FILE` | `services.gradient.githubApp.webhookSecretFile` | Path to the webhook secret file |

The nix module also exposes `services.gradient.githubApp.enable` as the master
switch — set it to `true` to wire the credentials into the systemd unit.

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `400 manifest state invalid or expired` | The CSRF state token is older than 10 minutes; re-start the manifest flow. |
| `404 Pending credentials` on `/admin/github-app?ready=1` | The temporary in-memory store dropped them (server restart, another tab consumed them, or you arrived more than 10 minutes after creation). Re-run the flow. |
| `500 github exchange failed: ... 401` | GitHub rejected the temporary code (typically because more than ~10 seconds elapsed before the callback fired, or the App was deleted on GitHub before the callback completed). Re-run the flow. |
| `403 superuser required` | The signed-in account doesn't have the `superuser` flag. Set it on the user row directly (no admin UI for this yet). |
