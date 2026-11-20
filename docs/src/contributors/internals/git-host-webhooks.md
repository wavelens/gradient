<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# Git Host Webhooks

Git hosts send events to the endpoints in `gradient-web/src/endpoints/git_host_hooks/`. These routes use no session. Each request must prove its origin through the Git host's signature before the endpoint acts on it. A push event results in a `Queued` evaluation.

```mermaid
sequenceDiagram
    participant F as Git host
    participant H as git_host_hooks
    participant C as gradient_ci
    participant S as eval-dispatch
    F->>H: POST push (signed)
    H->>H: verify signature, match repository
    H->>C: fan_out_triggers -> apply_trigger
    C->>C: dedup, concurrency
    C->>C: trigger_evaluation: Commit + Queued evaluation
    S->>S: woken by the new evaluation, handing the evaluation to a worker
```

## Routes

| Route | Verification |
|---|---|
| `POST /api/v1/hooks/github` | `X-Hub-Signature-256` checked against `GRADIENT_GITHUB_APP_WEBHOOK_SECRET_FILE`. `503` until the App is fully configured |
| `POST /api/v1/hooks/{connection_id}` | Gitea / Forgejo / GitLab connections. UUID for each. `gitea` / `forgejo`: HMAC from `X-Forgejo-Signature`, then `X-Gitea-Signature`. `gitlab`: constant-time comparison of `X-Gitlab-Token`. GitHub App connections respond with `404`. |
| `POST /api/v1/hooks/{git_host}/{project}/{connection_name}` | The alias for the project connection that `(project, name)` identifies. Values in the `git_host` segment select the payload format. Segment values equal to `github` return `400`. |

- Route `/api/v1/hooks/{connection_id}` can use the connection's `git_host_type` for the payload format.
- Connections with `receives_events` off and unknown connections return `404`.
- The crypt file can decrypt secrets on the server.
- Requests from other sources fail at `allowed_ips` with `403`.
- GitHub `installation` / `installation_repositories` events create or update instance connection `github-<login>` with `save_github_app_installation_and_link_tasks_when_new` (with `github-<installation_id>` if the name is in use). `deleted` events delete that connection.
- A newly inserted installation can link every unmanaged task without a connection whose GitHub repository belongs to the account, in the same transaction. Updates of an existing installation link nothing.
- Project connections created through the API or OAuth2 link the unmanaged project tasks on the same known host the same way (`insert_and_link_fitting_tasks`). Team connections, instance connections without a GitHub installation and declarative state never link tasks.

## GitHub App Events

GitHub App webhooks reach the following handlers:

| Event | Handler |
|---|---|
| `push` | Push chain below |
| `pull_request` | Pull request triggers |
| `release` | `route_github_app_release`: triggers with `releases_only` |
| `check_run` | `approval::handle_github_check_run` |
| `pull_request_review` | `approval::handle_pull_request_review` |
| `issue_comment` | `commands::handle_issue_comment`: comment commands |
| `installation`, `installation_repositories` | `handle_github_installation` |

## Push Chain

| Step | Code | Action |
|---|---|---|
| 1 | `fan_out_triggers` (`git_host_hooks/fanout.rs`) | Selects active `task_trigger` rows of type `ReporterPush` where the branch and tag globs match the push |
| 2 | `normalize_repo_url`, `event_repo_matches_task` | Strips a trailing `/` and `.git`, rewrites `git@` URLs. Compares `owner/repo` in lowercase and ignores the host |
| 3 | `gradient_ci::apply_trigger` | Drops a head commit with a [skip marker](../../concepts/projects-and-tasks.md#skip-a-commit) as `SkippedByCommitMessage`. Applies dedup and concurrency rules. A running evaluation can be aborted, or the new one parks in `Waiting`. A violation of `uq_evaluation_one_active_per_task` maps to `SkippedConcurrency` |
| 4 | `gradient_ci::trigger::trigger_evaluation` | Insert or update the `Commit` row for the project and hash. Keep the stored message if the new message is empty. Insert the `Queued` evaluation. Reset `last_check_at`. |
| 5 | `eval-dispatch` | Gets woken through `record_evaluation_created` and offers the evaluation to workers. A 5 s tick serves as the fallback |

## Approval of Pull Requests from Forks

Pull requests from forks wait for maintainers with `require_approval` in the trigger. Gradient does not create an evaluation of a commit until people with write access to the repository approve that commit.

| Step | Code | Action |
|---|---|---|
| 1 | `decide_pr_gate` (`git_host_hooks/fanout.rs`) | Block unless the head repository is equal to the base repository, or the sender can write to the repository. Treat an unknown fork state as a fork. |
| 2 | `is_commit_approved_by_maintainer_for_task` | Approved commits start evaluation immediately. Database errors mark the commit as unapproved. |
| 3 | `store_pending_approval_for_untrusted_fork_pull_request_commit_and_post_approval_check` | Store one pending `fork_pull_request_approval` row for each task, pull request and commit. Delete old pending rows for the same pull request. Record `fork_pull_request_approval.needs_maintainer_approval`. Post the `gradient/<task>: Approval` check. |
| 4 | `handle_github_check_run` (`git_host_hooks/approval.rs`) | A user with write permission can use the **Approve and run** button to approve the row of the check that was clicked. |
| 5 | `handle_pull_request_review` | An approval review from a user with write permission can approve only the pending row of the commit under review. GitHub can send that commit as `review.commit_id`. Gitea and Forgejo don't send the commit under review. Gradient can use `pull_request.head.sha` instead. A review for a commit without a pending row can't change anything. |
| 6 | `approve_newest_pending_approval_and_start_its_evaluation` | `/gradient approve` and `/gradient run` from users with write permission have no commits. These comments approve the newest pending row of the pull request. |
| 7 | `approve_approval_and_start_approved_commit_evaluation` | Load the task and active triggers first. Mark the row as approved and create an evaluation in one transaction. Roll back the transaction and keep the row pending if no new evaluation can result. Record `fork_pull_request_approval.evaluation_started_after_approval` after the commit, not before. |

## Related

- [Connect GitHub](../../guides/github.md) covers the initial setup
- [Events and Webhooks](../../reference/events.md) documents outbound events
