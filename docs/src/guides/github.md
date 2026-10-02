# Connect GitHub

Evaluations on every push and pull request, with status checks on each commit, through one GitHub App per Gradient instance.

**Requirements:**

- A Gradient account with the superuser flag
- Admin rights on the GitHub user or organization owning the repositories
- A task with a repository URL pointing to GitHub, see [First Project](../get-started/first-project.md)

## 1. Register the GitHub App

This step is needed once per instance. Open `https://gradient.example.com/admin/github-app`, then **Create on GitHub** and confirm on GitHub. GitHub Enterprise instances need their host entered first.

The Gradient page is then showing three credentials once: the **App ID**, the **private key** and the **webhook secret**. Store the key and the secret as files on the server.

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

Install the App from its GitHub page on the account owning the repositories. A project's **Integrations** page is linking to the App page while no installation is linked.

Gradient is matching the granted repositories against task repository URLs. `https://`, SSH and `github:owner/repo` URLs all match. Each matching project is receiving a `github-<account>` integration pair.

Projects created after the installation need the integration by hand. Open **Integrations -> New Integration** and pick **Git Host** GitHub. Enter the **App Installation ID** from the App's installation page on GitHub.

## 4. Wire the Task

Gradient is adding a **Push (reporter)** trigger and a **Git Host Status Report** action automatically to a task with a matching repository URL. Other tasks need both added by hand.

- **Triggers -> New Trigger**: **Push (reporter)** and, for pull requests, **Pull Request (reporter)**, each with the `github-<account>` integration.
- **Actions -> New Action**: **Git Host Status Report** with the `github-<account>` integration.

## Verify Deployment

- A push to the repository is starting an evaluation within seconds.
- The commit on GitHub is showing Gradient's status checks.

## Pull Requests

The automatic setup is covering pushes only. Pull requests need the **Pull Request (reporter)** trigger from step 4.

| Action on GitHub | Effect |
|---|---|
| Open or update a pull request | Evaluation of the pull request's head commit |
| Comment `/gradient run` | New evaluation of the pull request |
| Comment `/gradient approve` or approve the review | Release of a pull request from a fork waiting for maintainer approval |

The approval gate is a setting of the **Pull Request (reporter)** trigger: **Require maintainer approval for PRs from non-writers**.

??? note "Registering the App by Hand"
    Register the App following [GitHub's documentation](https://docs.github.com/en/apps/creating-github-apps/registering-a-github-app/registering-a-github-app) where the manifest flow does not fit. The table is holding the values for the registration.

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
