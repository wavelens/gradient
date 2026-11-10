<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# Connect Gitea or Forgejo

Pushes and pull requests trigger evaluations. Commit statuses appear back on the Git host.

**Requirements:**

- A task with a repository URL pointing to the Gitea or Forgejo instance, see [First Project](../get-started/first-project.md)
- Admin rights on the Gitea or Forgejo instance, or on the repository's owner, so a bot account can be created

## Inbound and Outbound

Gradient exchanges data with the Git host in two directions. Each direction is a separate integration of the project.

```mermaid
flowchart LR
    host["Gitea / Forgejo"] -- webhook --> inbound["Inbound integration"]
    inbound --> trigger["Push (reporter) trigger"]
    trigger --> eval["Evaluation"]
    eval --> action["Git Status action"]
    action --> outbound["Outbound integration"]
    outbound -- "API token" --> host
```

| | Inbound | Outbound |
|---|---|---|
| Direction | Git host -> Gradient | Gradient -> Git host |
| Holds | Webhook secret, allowed source IPs | Endpoint URL, access token of a bot account |
| Picked by | **Push (reporter)** and **Pull Request (reporter)** triggers | **Git Status** and **PR** actions |
| Covers | Evaluations on push, pull request and release. `/gradient` comment commands | Commit statuses. Reactions to `/gradient` comments. Writer check of pull request authors. [Flake update](flake-updates.md) pull requests |
| Without | Git host events never reach Gradient | No status on commits. `/gradient` comments ignored. All pull request authors count as non-writers |

- The inbound integration can receive webhooks from all repositories of the project's tasks. Events match a task by the `owner/repo` part of the repository URL.
- Outbound calls act as the token's account. Statuses and comments appear under that account's name.
- Writer checks and comment reactions use the outbound integration of the task's **Git Status** action.

## 1. Create a Bot Account

Gitea and Forgejo tie tokens to individual users. A dedicated bot account keeps statuses and comments apart from personal accounts.

| Repository owner | Access for the bot account |
|---|---|
| User | In each repository, open **Settings -> Collaborators** and grant the **Admin** permission |
| Organization | Make the bot a member of a team with **Administrator Access** to the repositories |

Gitea and Forgejo only answer permission queries for repository admins, so the writer check on pull request authors needs admin rights. Tasks without a **Pull Request (reporter)** trigger work with **Write** access alone.

The token needs these scopes.

| Scope | Needed for |
|---|---|
| `write:repository` | Commit statuses, the writer check, flake update branches and pull requests |
| `write:issue` | Reactions and replies on pull request comments |
| `read:user` | Commit author of flake update commits |

=== "UI"

    Sign in as the bot account and open **Settings -> Applications -> Generate New Token**. Pick **Repository and Organization Access** **All** for private repositories, then the scopes above.

=== "CLI"

    ```sh
    # on the Gitea host, as the Gitea service user (forgejo instead of gitea for Forgejo)
    gitea admin user create --username gradient-bot \
      --email gradient-bot@example.com --random-password
    gitea admin user generate-access-token --username gradient-bot \
      --token-name gradient --scopes write:repository,write:issue,read:user --raw
    ```

## 2. Connect the Repository

1. Click **Connect repository** in the project. Enter the repository URL and click **Create task**.
2. Click **Connect gitea.example.com** in the **Events and Status** step. Select Gitea or Forgejo. Keep the base URL as is and paste the bot token. Click **Connect**.
3. Click **Open task**. The first evaluation has already begun at task creation.

**Connect** can run all of these steps at once.

- Gradient can create a webhook receiver and a status reporter for the Git host.
- Tasks build pushes and pull requests and report commit statuses.
- With admin rights on the token, Gradient can install a webhook and a deploy key in the repository.

Gradient shows the webhook URL and secret once if the token lacks admin rights. Add them under **Settings -> Webhooks -> Add Webhook** on the Git host.

| Field | Value |
|---|---|
| Target URL | The webhook URL |
| HTTP method, content type | `POST`, `application/json` |
| Secret | The secret |
| Trigger on | Custom events: **Push**, **Pull Request**, **Issue Comment**, **Pull Request Comment**, **Pull Request Review**, **Release** |

Add the deploy key from step **Access** under **Settings -> Deploy Keys** for a repository URL over SSH, then click **Test access**.

Existing tasks keep the same settings under **Task settings -> Repository**. Tokens with admin permissions have the **Install webhook** button and the **Install deploy key** button.

## OAuth2 Connection

The administrator can register Gradient once as an OAuth2 application on a Gitea or Forgejo instance. Project members can then connect a Git host without pasting a token.

1. Open the **Settings -> Applications -> OAuth2 Applications** of the user or organization on the Git host.
2. Set the redirect URI to `https://gradient.example.com/api/v1/integrations/oauth/callback` and create a confidential client. Save the client secret to a file.
3. Configure Gradient on NixOS.

```nix
services.gradient.giteaApp = {
  enable = true;
  instanceBaseUrl = "https://codeberg.org";
  gitHostType = "forgejo"; # (1)!
  clientId = "3f1b6c2a-9d4e-4a7b-8c1d-2e5f7a9b0c13";
  clientSecretFile = "/run/secrets/gitea-oauth-client-secret";
};
```

1.  `gitea` for Gitea.

Project members:

1. Open **Integrations** in the project. Click **Connect with Forgejo (codeberg.org)**.
2. Enter a name and click **Continue**. Approve access on the Git host.
3. Gradient set up a webhook receiver and a status reporter with this name. The browser went back to the integrations page.

- Forgejo and Gitea don't add scopes to OAuth2 tokens. An OAuth2 token can access the full account that approved it. Approve the token with the bot account from Step 1.
- Gradient can refresh the access token automatically. The status reporter can show **Automatic refresh of the token**. Enter a token in **Edit** to replace the OAuth2 token and end the refresh.
- Gradient can install webhooks on the repositories of tasks that connect to a new integration. The authorizing account must have admin rights on each repository.
- A Gradient server can have at most one OAuth2 application, so only one Gitea or Forgejo instance can use this connection.

## Declarative Setup

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
    endpoint_url = "https://gitea.example.com"; # (3)!
    access_token_file = "/run/secrets/gitea-token"; # (4)!
    created_by = "alice";
  };
};
```

1.  `forgejo` for Forgejo.
2.  Any random string, e.g. `openssl rand -hex 32`. Set the same value in the Git host webhook.
3.  Base URL of the Git host, without `/api/v1`.
4.  The bot's token from above, as the only content of the file.

The webhook URL is `https://gradient.example.com/api/v1/hooks/gitea/acme/gitea-in` (`forgejo` instead of `gitea` for Forgejo).

Declaratively defined tasks ship without a trigger or action. Add both to the task's `triggers` and `actions` lists.

```nix
services.gradient.state.tasks.app = {
  project = "acme";
  repository = "https://gitea.example.com/acme/app.git";
  created_by = "alice";
  triggers = [
    { type = "reporter_push"; integration = "gitea-in"; } # (1)!
    { type = "reporter_pull_request"; integration = "gitea-in"; } # (2)!
  ];
  actions = [
    {
      name = "report-status";
      type = "git_host_status_report";
      config.integration = "gitea-out"; # (3)!
    }
  ];
};
```

1.  The `integration` field at trigger level points to the inbound integration. Put branch and tag filters into `config`, see [Trigger Types](../reference/state.md#trigger-types).
2.  Optional. Covers evaluations of pull requests. Fork pull requests stay pending until a maintainer gives approval.
3.  The `config.integration` field points to the outbound integration.

## Verify Deployment

- An evaluation starts within seconds after a push.
- The Git host reports `200` for each webhook delivery.
- The Git host shows a commit status from Gradient.

## Pull Requests

| Git host action | Result |
|---|---|
| Open or update a pull request | The head commit of the pull request get evaluated |
| Comment `/gradient run` | A new evaluation of the pull request start |
| Comment `/gradient approve` or approve the review | A pull request from a fork that waited for maintainer approval get released |
| Comment `/gradient summary` | One comment with the built, failed and unchanged packages, updated after every push |

The **Pull Request (reporter)** trigger include a setting called **Require maintainer approval for PRs from non-writers** (`require_approval` in `config`). This setting controls the approval check.

Pull requests from the [AGit flow](https://docs.gitea.com/usage/issues-prs/agit/) (`git push origin HEAD:refs/for/main`) get evaluated too.

- Read access is enough for such a push. These pull requests wait for approval the same way fork pull requests do, unless a repository writer pushed them.
- Branch filters match the AGit topic on Forgejo and `refs/pull/<number>/head` on Gitea.

Only repository writers can send `/gradient` commands. Accepted commands receive a 👀 reaction, rejected ones a 😕.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `401` in the Git host's delivery log | The secret on the two sides differ. Re-enter the secret on both sides (**Edit -> Webhook Secret** in Gradient) |
| `403 forbidden_source_ip` | Add the Git host's address to the integration's allowed source IPs |
| `404` | The webhook URL contain a wrong project or integration name, or the inbound integration lack a secret |
| `200`, but no evaluation | No task trigger use this integration, or no task repository URL match |
| No status on the commit | The task lack a **Git Status** action, or the token lack `write:repository` |
| 😕 on a maintainer's `/gradient` comment | The bot account lack admin access to the repository |

## Next Steps

- [Actions](actions.md) - send mail, make web requests and open flake update pull requests
- [Declarative State](../reference/state.md) - every option for tasks, triggers and actions
- [Connect GitLab](gitlab.md) - the same setup, adapted for GitLab
