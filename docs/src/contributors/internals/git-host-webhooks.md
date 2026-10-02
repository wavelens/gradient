# Git Host Webhooks

`gradient-web/src/endpoints/git_host_hooks/` is receiving Git host events. The routes carry no session. Each delivery is proving its origin with the Git host's signature. A push is ending as a `Queued` evaluation.

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
| `POST /api/v1/hooks/github` | `X-Hub-Signature-256` against `GRADIENT_GITHUB_APP_WEBHOOK_SECRET_FILE`. `503` until the App is fully configured |
| `POST /api/v1/hooks/{git_host}/{project}/{integration_name}` | `gitea` / `forgejo`: HMAC from `X-Forgejo-Signature`, then `X-Gitea-Signature`. `gitlab`: `X-Gitlab-Token` compared in constant time. `github` is answering `400` |

- The generic route is finding the integration by `(project, inbound, name)`, without `git_host_type`.
- One inbound row is serving all three Git hosts.
- The server is decrypting the secret with the crypt file.
- `allowed_ips` is rejecting other sources with `403`.
- GitHub `installation` / `installation_repositories` events upsert or clear `github_installation` and seed the `github-<login>` integration pair (`github-<installation_id>` without a login).

## GitHub App Events

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
| 1 | `fan_out_triggers` (`git_host_hooks/fanout.rs`) | Active `task_trigger` rows of type `ReporterPush` whose branch and tag globs match |
| 2 | `normalize_repo_url`, `event_repo_matches_task` | Stripping `.git` and a trailing `/`, rewriting `git@` URLs. Comparing lowercased `owner/repo`, ignoring the host |
| 3 | `gradient_ci::apply_trigger` | Dedup and concurrency, aborting the running evaluation or parking the new one in `Waiting`. A violation of `uq_evaluation_one_active_per_task` is mapping to `SkippedConcurrency` |
| 4 | `gradient_ci::trigger::trigger_evaluation` | Inserting the `Commit` row and a `Queued` evaluation, setting `task.force_evaluation`, resetting `last_check_at` |
| 5 | `eval-dispatch` | Woken by the created evaluation (`record_evaluation_created`), offering the evaluation to workers. The 5 s tick is the fallback |

## Related

- [Connect GitHub](../../guides/github.md): the setup side
- [Events and Webhooks](../../reference/events.md): outbound events
