# Connect GitHub

A GitHub App links one Gradient instance to GitHub. With this connection in place, every push and pull request can trigger evaluations. Each commit then shows a status check.

**Requirements:**

- A Gradient account with the superuser flag.
- Admin rights on the GitHub user or organization that owns the repositories.
- A task with a GitHub repository URL, see [First Project](../get-started/first-project.md).

## 1. Register the GitHub App

Register the app once per instance. GitHub Enterprise users first enter their host address. Then open `https://gradient.example.com/admin/github-app`, click **Create on GitHub** and confirm the registration on GitHub.

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

Install the App from its GitHub page on the account that owns the repositories. Until an installation is linked, the **Integrations** page of a project links to the App page.

Gradient compares the granted repositories with task repository URLs. `https://`, SSH and `github:owner/repo` URLs all match. Gradient links each matching project to a `github-<account>` integration pair.

Projects created after the installation lack the integration. Open **Integrations -> New Integration** and pick **Git Host** GitHub. Enter the **App Installation ID** from the App's installation page on GitHub.

## 4. Wire the Task

A task links the two integrations: triggers use the inbound one, actions use the outbound one. Both carry the name `github-<account>`.

=== "UI"

    A task whose repository URL matches gets a **Push (reporter)** trigger and a **Git Status** action without manual work. All other tasks require adding both by hand.

    - **Triggers -> New Trigger**: add **Push (reporter)**, plus **Pull Request (reporter)** for pull requests, each bound to the `github-<account>` integration.
    - **Actions -> New Action**: add **Git Status** bound to the `github-<account>` integration.

=== "Declarative"

    Declared tasks receive no trigger or action automatically. Add both to the task's `triggers` and `actions` lists.

    ```nix
    services.gradient.state.tasks.app = {
      project = "acme";
      repository = "https://github.com/acme/app.git";
      created_by = "alice";
      triggers = [
        { type = "reporter_push"; integration = "github-acme"; } # (1)!
        { type = "reporter_pull_request"; integration = "github-acme"; }
      ];
      actions = [
        {
          name = "report-status";
          type = "git_host_status_report";
          config.integration = "github-acme"; # (2)!
        }
      ];
    };
    ```

    1.  The trigger-level `integration` field points at the inbound integration of the App installation.
    2.  The `config.integration` field points at the outbound integration of the App installation.

## Verify Deployment

- A push to the repository triggers an evaluation within seconds.
- Gradient reports its status on the GitHub commit page.

## Pull Requests

Push events work with the automatic setup. Pull requests require the **Pull Request (reporter)** trigger from step 4.

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
| Pushes arrive but no evaluation starts | No task repository URL matches the pushed repository |
| `403 forbidden_source_ip` | The integration's allowed source IPs do not cover GitHub's `hooks` ranges from `https://api.github.com/meta` |

## Next Steps

- [Actions](actions.md): mail, web requests and more after an evaluation
- [Projects and Tasks](../concepts/projects-and-tasks.md): triggers, actions and concurrency
