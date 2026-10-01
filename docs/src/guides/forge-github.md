# Connect GitHub

Evaluations on every push and pull request, with status checks on each commit, through one GitHub App per Gradient instance.

**Requirements:**

- A Gradient account with the superuser flag
- Admin rights on the GitHub user or organization that owns the repositories
- A task whose repository URL points to GitHub, see [First Project](../get-started/first-project.md)

## 1. Register the GitHub App

Once per instance. Open `https://gradient.example.com/admin/github-app`, then **Create on GitHub** and confirm on GitHub. For GitHub Enterprise, enter the host first.

Back in Gradient, the page shows three credentials once: the **App ID**, the **private key** and the **webhook secret**. Store the key and the secret as files on the server.

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

Install the App from its GitHub page on the account that owns the repositories. A project's **Integrations** page links that page for as long as no installation is linked.

Gradient matches the granted repositories against task repository URLs (`https://`, SSH and `github:owner/repo` all match) and creates a `github-<account>` integration pair in each matching project.

For a project created after the installation: **Integrations -> New Integration**, forge GitHub, with the **Installation ID** from the App's installation page on GitHub.

## 4. Wire the Task

A task with a matching repository URL gets a **Push (reporter)** trigger and a **Forge Status Report** action automatically. Otherwise, on the task:

- **Triggers -> New Trigger**: **Push (reporter)** and, for pull requests, **Pull Request (reporter)**, each with the `github-<account>` integration.
- **Actions -> New Action**: **Forge Status Report** with the `github-<account>` integration.

## Verify Deployment

- A push to the repository starts an evaluation within seconds.
- The commit on GitHub shows Gradient's status checks.

## Pull Requests

The automatic setup covers pushes only; pull requests need the **Pull Request (reporter)** trigger from step 4.

| Action on GitHub | Effect |
|---|---|
| Open or update a pull request | Evaluates the pull request's head commit |
| Comment `/gradient run` | Starts an evaluation of the pull request |
| Comment `/gradient approve` or approve the review | Releases a pull request from a fork waiting for maintainer approval |

The approval gate is a task setting: **Require maintainer approval for PRs from non-writers**.

??? note "Registering the App by Hand"
    When the manifest flow does not fit, register the App following [GitHub's documentation](https://docs.github.com/en/apps/creating-github-apps/registering-a-github-app/registering-a-github-app) with these values:

    | Setting | Value |
    |---|---|
    | Webhook URL | `https://gradient.example.com/api/v1/hooks/github` |
    | Setup URL | `https://gradient.example.com/admin/github-app` (optional) |
    | Permissions | `metadata: read`, `contents: read`, `pull_requests: write`, `issues: write`, `statuses: write`, `checks: write` |
    | Events | `push`, `pull_request`, `release`, `check_run`, `issue_comment`, `pull_request_review` |

## Troubleshooting

| Symptom | Fix |
|---|---|
| `400 manifest state invalid or expired` | More than 10 minutes passed; start step 1 again |
| `404 Pending credentials` after creating the App | The credentials were already shown or the server restarted; start step 1 again |
| `403 superuser required` | The account lacks the superuser flag |
| An approving review does not release a fork pull request | The App predates the `pull_request_review` event; enable **Pull request review** under the App's **Permissions & events** |
| Push arrives, no evaluation starts | No task repository URL matches the pushed repository |
| `403 forbidden_source_ip` | The integration's allowed source IPs miss GitHub's `hooks` ranges from `https://api.github.com/meta` |

## Next Steps

- [Actions](actions.md): mail, web requests and more after an evaluation
- [Projects and Tasks](../concepts/projects-and-tasks.md): triggers, actions and concurrency
