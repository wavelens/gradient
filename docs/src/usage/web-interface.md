# Web Interface

The web interface is served at the configured domain and communicates exclusively with the REST API.

## Getting Started

1. Navigate to `/account/register` to create the first user account.
2. Log in at `/account/login`.
3. Create an organization — organizations are the top-level grouping unit, each with its own projects, servers, caches, and members.

## Projects

A project maps to a Git repository and defines which Nix packages to evaluate.

**Creating a project:**

1. Open the organization and click **New Project**.
2. Set the **Repository URL** (SSH or HTTPS), **Branch**, and **Evaluation Wildcard** — a Nix attribute path such as `packages.x86_64-linux.*` or `checks.*`.
3. Save. If the repository is private, copy the organization's SSH public key from **Settings → SSH** and add it to the repository's deploy keys.

**Project detail page** shows the current entry-point builds (one card per top-level derivation) and a recent evaluation history table.

## Evaluations

Click **Start Evaluation** on the project detail page to queue a new run. Gradient clones the repository, runs `nix eval` against the wildcard, creates build rows for every derivation, then dispatches them to the configured build servers.

The evaluation log page shows:
- Status and duration
- A sidebar listing all builds with per-build status indicators
- The combined build log with ANSI colour support

Click **Abort** to cancel an in-progress evaluation.

## Settings

Organization and project settings are accessible via the **Settings** button on their respective pages. From there you can rename, manage members, add/remove servers and caches, and update the repository configuration.

API keys are managed under **Settings → API Keys** in the user menu.
