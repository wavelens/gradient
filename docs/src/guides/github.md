<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# Connect GitHub

A GitHub App links one Gradient instance to GitHub. With this connection in place, every push and pull request can trigger evaluations. Each commit then shows a status check.

**Requirements:**

- A Gradient account with the superuser flag.
- Admin rights on the GitHub user or organization that owns the repositories.
- A task with a GitHub repository URL, see [First Project](../get-started/first-project.md).

## 1. Register the GitHub App

Register the App once for each instance. Open the **Git hosts** tab in **Admin** of Gradient. Users of GitHub Enterprise enter the host address first. Click **Create on GitHub** and confirm the registration on GitHub.

Gradient now shows the **App ID**, the **private key** and the **webhook secret** one time only. Save the key and the secret as files on the server.

## 2. Configure the Server

```nix
services.gradient.githubApp = {
  enable = true;
  id = 123456; # (1)!
  privateKeyFile = "/run/secrets/gradient-github-app.pem";
  webhookSecretFile = "/run/secrets/gradient-github-app-webhook";
};
```

1. The App ID obtained in step 1.

## 3. Install the App

Install the App from the GitHub page of the App on the account with the repository. Each installation is a connection to a Git host named `github-<account>` with the instance as owner. Find the connection on the **Git hosts** tab under **Admin**. All projects can use the connection. The connection can receive events and report status. Remove the installation on GitHub. The connection is gone.

## 4. Choose the Connection on the Task

Tasks map to one Git host connection each. Push triggers and pull request triggers fire for tasks on an event from their Git host connection. Commit statuses go through the same connection.

=== "UI"

    Open the **Settings -> Repository** page of the task. Select `github-<account>` in **Git host connection**, turn on **Report commit status** and save. Users who can edit the task can make this choice. **Test status report** posts a `pending` status to the head commit of the last evaluation.

    Add push triggers and pull request triggers on the **Triggers** page of the task.

    The last step of **Connect repository** also offers the same triggers.

=== "Declarative"

    A declarative task has no connection and no triggers by default. Declare a connection for the app installation with the installation ID, then add both triggers to the task. The installation webhook can reuse the declared connection.

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

## Verify Deployment

- A push to the repository triggers an evaluation within seconds.
- Gradient reports its status on the GitHub commit page.

## Pull Requests

Push events require the **Push (reporter)** trigger from step 4. Pull Requests require the **Pull Request (reporter)** trigger.

| Action on GitHub | Effect |
|---|---|
| Open or update a pull request | Evaluation of the pull request's head commit |
| Comment `/gradient run` | New evaluation of the pull request |
| Comment `/gradient approve` or approve the review | Release of a pull request from a fork waiting for maintainer approval |
| Comment `/gradient summary` | One comment with the built, failed and unchanged packages, updated after every push |

Fork pull requests wait for maintainer approval before they run. To enable this check, set **Require maintainer approval for PRs from non-writers** on the **Pull Request (reporter)** trigger.

## Merge Queue

Merge queue entries count as a push to the target branch of the queue. A **Push (reporter)** trigger with a branch filter on the target branch can start the evaluation.

1. Add the status check `gradient/<task>: Evaluation` to the required checks of the target branch.
2. Enable **Require merge queue** on the same branch ruleset.

GitHub can merge an entry after a passed check, or drop the entry after a failed one.

??? note "Registering the App by Hand"
    Use [GitHub's documentation](https://docs.github.com/en/apps/creating-github-apps/registering-a-github-app/registering-a-github-app) to register the App by hand if the manifest flow does not fit. Enter these values during registration.

    | Setting | Value |
    |---|---|
    | Webhook URL | `https://gradient.example.com/api/v1/hooks/github` |
    | Permissions | `metadata: read`, `contents: read`, `pull_requests: write`, `issues: write`, `statuses: write`, `checks: write`, `merge_queues: read` |
    | Events | `push`, `pull_request`, `release`, `check_run`, `issue_comment`, `pull_request_review`, `merge_group` |

## Troubleshooting

| Symptom | Fix |
|---|---|
| `400 manifest state invalid or expired` | Over 10 minutes elapsed. Repeat step 1 |
| `404 Pending credentials` after App creation | The credentials got shown once already, or the server restarted. Repeat step 1 |
| `403 superuser required` | The account lacks the superuser flag |
| An approving review does not release a fork pull request | The App predates the `pull_request_review` event. Enable **Pull request review** under the App's **Permissions & events** |
| Merge queue entries wait for the Gradient check without end | The App predates the `merge_group` event. Grant **Merge queues** read access and enable **Merge group** under the App's **Permissions & events** |
| Pushes arrive but no evaluation starts | No **Git host connection** on the task, no push trigger, or a repository URL that doesn't match the pushed repository |
| No status on the commit | **Report commit status** is off on the task's **Repository** page |
| `403 forbidden_source_ip` | The connection's allowed source IPs don't cover the GitHub `hooks` ranges from `https://api.github.com/meta` |

## Next Steps

- [Actions](actions.md): mail, web requests and more after an evaluation
- [Projects and Tasks](../concepts/projects-and-tasks.md): triggers, actions and concurrency
