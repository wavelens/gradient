# Connect GitHub

Evaluations on every push and pull request, with a status on each commit, through one GitHub App per Gradient instance.

**Requirements:**

- A Gradient account with the superuser flag.
- Admin rights on the GitHub user or organization owning the repositories.
- A task with a repository URL pointing to GitHub, see [First Project](../get-started/first-project.md).

## 1. Register the GitHub App

This step is needed once per instance. Open `https://gradient.example.com/admin/github-app`, then **Create on GitHub** and confirm on GitHub. GitHub Enterprise instances need their host entered first.

The Gradient page will then show three credentials once: the **App ID**, the **private key** and the **webhook secret**. Store the key and the secret as files on the server.

## 2. Configure the Server

```nix
services.gradient.githubApp = {
  enable = true;
  id = 123456; # (1)!
  privateKeyFile = "/run/secrets/gradient-github-app.pem";
  webhookSecretFile = "/run/secrets/gradient-github-app-webhook";
};
```

1.  The App ID from step 1.

## 3. Install the App

Install the App from its GitHub page on the account owning the repositories. A project's **Integrations** page will link to the App page while no installation is linked.

Gradient will match the granted repositories against task repository URLs. `https://`, SSH and `github:owner/repo` URLs all match. Each matching project will receive a `github-<account>` integration pair.

Projects created after the installation need the integration by hand. Open **Integrations -> New Integration** and pick **Git Host** GitHub. Enter the **App Installation ID** from the App's installation page on GitHub.

## 4. Wire the Task

Tasks connect the two integrations. Triggers point at the inbound integration, actions at the outbound integration. Both share the name `github-<account>`.

=== "UI"

    Tasks with a matching repository URL get a **Push (reporter)** trigger and a **Git Status** action automatically. Other tasks need both added by hand.

    - **Triggers -> New Trigger**: **Push (reporter)** and, for pull requests, **Pull Request (reporter)**, each with the `github-<account>` integration.
    - **Actions -> New Action**: **Git Status** with the `github-<account>` integration.

=== "Declarative"

    Declared tasks get no automatic trigger or action. Both belong in the task's `triggers` and `actions` lists.

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

    1.  Trigger-level `integration`, naming the inbound integration of the App installation.
    2.  `config.integration`, naming the outbound integration of the App installation.

## Verify Deployment

- A push to the repository will start an evaluation within seconds.
- The commit on GitHub will show Gradient's status checks.

## Pull Requests

The automatic setup can cover pushes only. Pull requests need the **Pull Request (reporter)** trigger from step 4.

| Action on GitHub | Effect |
|---|---|
| Open or update a pull request | Evaluation of the pull request's head commit |
| Comment `/gradient run` | New evaluation of the pull request |
| Comment `/gradient approve` or approve the review | Release of a pull request from a fork waiting for maintainer approval |

The approval check is a setting of the **Pull Request (reporter)** trigger: **Require maintainer approval for PRs from non-writers**.

??? note "Registering the App by Hand"
    Register the App following [GitHub's documentation](https://docs.github.com/en/apps/creating-github-apps/registering-a-github-app/registering-a-github-app) where the manifest flow does not fit. The values for the registration are in the table below.

    | Setting | Value |
    |---|---|
    | Webhook URL | `https://gradient.example.com/api/v1/hooks/github` |
    | Permissions | `metadata: read`, `contents: read`, `pull_requests: write`, `issues: write`, `statuses: write`, `checks: write` |
    | Events | `push`, `pull_request`, `release`, `check_run`, `issue_comment`, `pull_request_review` |

## Troubleshooting

| Symptom | Fix |
|---|---|
| `400 manifest state invalid or expired` | More than 10 minutes passed. Start step 1 again |
| `404 Pending credentials` after creating the App | The credentials were already shown or the server restarted. Start step 1 again |
| `403 superuser required` | The account is lacking the superuser flag |
| An approving review does not release a fork pull request | The App is older than the `pull_request_review` event. Enable **Pull request review** under the App's **Permissions & events** |
| Push arriving, no evaluation running | No task repository URL is matching the pushed repository |
| `403 forbidden_source_ip` | The integration's allowed source IPs miss GitHub's `hooks` ranges from `https://api.github.com/meta` |

## Next Steps

- [Actions](actions.md): mail, web requests and more after an evaluation
- [Projects and Tasks](../concepts/projects-and-tasks.md): triggers, actions and concurrency
