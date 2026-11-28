<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# Connect GitHub

A GitHub App can link a Gradient instance to GitHub. With this connection in place, every push and pull request can trigger evaluations. Each commit then shows a status check.

## GitHub App

**Requirements:**

- A Gradient account with the superuser flag.
- Admin rights on the GitHub user or organization that owns the repositories.
- A task with a GitHub repository URL, see [First Project](../get-started/first-project.md).

### 1. Register the GitHub App

Register an App once per instance in the **GitHub App** section of **Administration -> Git host connections**.

1. Enter the GitHub organization in **Organization**. Leave the field empty for a personal account.
2. Enter the host in **Advanced -> Host for GitHub Enterprise** for GitHub Enterprise.
3. Click **Create on GitHub** and confirm the registration on GitHub.

Gradient shows **App ID**, **Private key (PEM)**, and **Webhook secret** only once, next to the `services.gradient.githubApp` snippet. Store the key and secret as files on the server.

### 2. Configure the Server

```nix
services.gradient.githubApp = {
  enable = true;
  id = 123456; # (1)!
  privateKeyFile = "/run/secrets/gradient-github-app.pem";
  webhookSecretFile = "/run/secrets/gradient-github-app-webhook";
};
```

1. The App ID obtained in step 1.

### 3. Install the App

Click **Install on GitHub** in the **GitHub App** section. Select the account that has the repository. The GitHub servers then redirect the browser back to **Git host connections**.

| Message | Meaning |
|---|---|
| "Installed on <account>" | Installation is active. The tasks that Gradient linked to the installation open from the link. |
| "Ask the owner of the account to approve the installation on GitHub." | Installations from a non-owner await approval on GitHub. |
| "GitHub doesn't report the installation yet." | Events from GitHub haven't arrived yet. **Open app on GitHub** leads to the App. Failed deliveries show under **Advanced** in the App settings. Reload the list with **Check again**. |
| "The GitHub App is on GitHub, but this server doesn't have it yet." | The server configuration from step 2 isn't active yet. Apply it and restart the server, then open the page again. |

- Each installation is a connection named `github-<account>` with the instance as owner.
- Tasks only use that connection, if they have [repository grant](../concepts/projects-and-tasks.md#repository-grants) for their repository. Superusers create grants. Declared tasks get grants from the state file. Connections receive events and report status.
- Gradient links a new installation to every task that has no connection, a repository in the installation account and a repository grant for that repository. A new repository grant can link the same task later. That task is unmanaged. The task has no connection and has the target repository of the grant.
- All installations appear under the App with their account, last event and task count.
- Deletions of a GitHub installation remove the connection.

**Connect repository** and the **Repository** page of the task both offer **Install on GitHub** when no installation can fit the repository. Other users see "An instance admin can install the GitHub App on <account>".

Both pages open GitHub in the same tab. The next step is a redirect from GitHub back to **Git host connections** with a link to the tasks for the new installation. An instance administrator of a server without the GitHub App can get **Set up GitHub App** in **Connect repository**.

### 4. Choose the Connection on the Task

Tasks map to one Git host connection each. Push triggers and pull request triggers fire for tasks on an event from their Git host connection. Commit statuses go through the same connection.

=== "UI"

    Gradient can pick the fitting installation for a new task with a GitHub repository. **Report commit status** is then on.

    Steps from Connection to Commit Status map to one section each on the **Repository** page of a task. A status and one action appear in each section.

    | Section | Actions |
    |---|---|
    | **Connection** | Choose another installation. Use **Install on GitHub** if no installation matches. |
    | **Incoming Events** | **Add trigger** for the push and pull request triggers |
    | **Commit Status** | Turn **Report commit status** on or off. **Test the status report** can post a `pending` status to the head commit of the last evaluation. |

    The **Triggers** page and the last step of **Connect repository** also offer the same triggers.

=== "Declarative"

    A declarative task has no connection and no triggers by default. Declare a connection for the app installation with the installation ID, then add both triggers to the task. The installation webhook can reuse the declared connection.

    Declarative state does not select or link a connection on its own. Declared values always take precedence. A declared installation can report status by default. Keep the task repository under `account_login`.

    ```nix
    services.gradient.state.git_host_connections.github-acme = {
      owner = "instance";
      git_host_type = "github";
      installation_id = 12345678; # (1)!
      account_login = "acme";
      reports_status = true;
      created_by = "alice";
    };

    services.gradient.state.tasks.app = {
      project = "acme";
      repository = "https://github.com/acme/app.git";
      created_by = "alice";
      git_host_connection = "github-acme";
      report_status = true;
      triggers = [
        { type = "reporter_push"; }
        { type = "reporter_pull_request"; }
      ];
    };
    ```

    1.  ID of the app installation. Find this number at the end of the URL in the settings for the app installation on GitHub.

### Verify Deployment

- A push to the repository triggers an evaluation within seconds.
- Gradient reports its status on the GitHub commit page.

### Pull Requests

Push events require the **Push (reporter)** trigger from step 4. Pull Requests require the **Pull Request (reporter)** trigger.

| Action on GitHub | Effect |
|---|---|
| Open or update a pull request | Evaluation of the pull request's head commit |
| Comment `/gradient run` | New evaluation of the head commit. Fork pull requests get no evaluation while maintainer approval is on, even when their authors can push. Gradient replies carry `/gradient run <commit>` for the head commit. |
| Comment `/gradient approve <commit>` | Evaluation of that commit in a fork pull request awaiting maintainer approval. Use the full 40-character hash for the commit. |
| Approve the review | Evaluation of the reviewed commit in a fork pull request awaiting maintainer approval. Reviews approve nothing if checks of write access take longer than 4 seconds. Use a `/gradient approve` comment with the full commit hash instead. |
| Comment `/gradient summary` | One comment with the built, failed and unchanged packages, updated after every push |
| Comment `/gradient abort` | Abort of all evaluations in progress on a pull request |

Commits in pull requests from forks wait for maintainer approval before the start of evaluation. The `gradient/<task>: Approval` check can show the **Approve and run** button. Each new commit must get a new approval. Set **Require maintainer approval for PRs from non-writers** with the **Pull Request (reporter)** trigger to enable this check.

A `/gradient approve` comment can approve only the commit with a full 40-character hash. A comment with an abbreviated commit hash can approve nothing. A comment without a commit approves the head commit only when Gradient recorded that head commit at least 60 seconds before the comment. Other comments with no commit don't get approval. Gradient may have recorded the head commit later, or not at all. Replies from Gradient then contain `/gradient approve <commit>` with the full hash of the head commit. New pushes require new approval. Fork pull requests run without project secrets, project SSH keys, and the `open_pr` and `start_evaluation` actions when the check is off.

### Merge Queue

Merge queue entries count as a push to the target branch of the queue. A **Push (reporter)** trigger with a branch filter on the target branch can start the evaluation.

1. Add the status check `gradient/<task>: Evaluation` to the required checks of the target branch.
2. Enable **Require merge queue** on the same branch ruleset.

GitHub can merge an entry after a passed check, or drop the entry after a failed one.

??? note "Registering the App by Hand"
    Use [GitHub's documentation](https://docs.github.com/en/apps/creating-github-apps/registering-a-github-app/registering-a-github-app) to register the App by hand if the manifest flow does not fit. Enter these values during registration.

    | Setting | Value |
    |---|---|
    | Webhook URL | `https://gradient.example.com/api/v1/hooks/github` |
    | Setup URL | `https://gradient.example.com/admin/git-host-connections` |
    | Permissions | `metadata: read`, `contents: read`, `pull_requests: write`, `issues: write`, `statuses: write`, `checks: write`, `merge_queues: read` |
    | Events | `push`, `pull_request`, `release`, `check_run`, `issue_comment`, `pull_request_review`, `merge_group` |

### Troubleshooting

| Symptom | Fix |
|---|---|
| `400 manifest state invalid or expired` | Over 10 minutes elapsed. Repeat step 1 |
| `404 Pending credentials` after App creation | The credentials got shown once already, or the server restarted. Repeat step 1 |
| `403 superuser required` | The account lacks the superuser flag |
| Approved reviews don't start evaluation of pull requests from forks | The App predates the `pull_request_review` event. Enable **Pull request review** under the App's **Permissions & events** |
| Merge queue entries wait for the Gradient check without end | The App predates the `merge_group` event. Grant **Merge queues** read access and enable **Merge group** under the App's **Permissions & events** |
| "GitHub doesn't report the installation yet." stays after **Check again** | App webhook doesn't arrive at `https://gradient.example.com/api/v1/hooks/github`. Check the App's **Advanced -> Recent Deliveries** on GitHub |
| "An instance admin can install the GitHub App on <account>" | Only instance administrators can install an App. An instance administrator needs to click **Install on GitHub** |
| Pushes arrive but no evaluation starts | The task's **Repository** page shows the failing step (no connection, no push trigger, no [grant](../concepts/projects-and-tasks.md#repository-grants) for the repository, or a repository outside the installation account) |
| `github-<account>` missing from the task's connection list, or `403 This task can't use Git host connection` | No grant for the task repository on the connection. Ask a superuser |
| No status on the commit | **Report commit status** is off on the task's **Repository** page |
| `403 forbidden_source_ip` | The connection's allowed source IPs don't cover the GitHub `hooks` ranges from `https://api.github.com/meta` |

## Next Steps

- [Actions](actions.md): mail, web requests and more after an evaluation
- [Projects and Tasks](../concepts/projects-and-tasks.md): triggers, actions and concurrency
