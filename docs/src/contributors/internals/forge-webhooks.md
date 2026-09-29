# Forge Webhooks

`gradient-web/src/endpoints/forge_hooks/` receives forge events. The routes carry no session; each delivery proves itself with the forge's signature, and a push ends as a `Queued` evaluation.

```mermaid
sequenceDiagram
    participant F as Forge
    participant H as forge_hooks
    participant C as gradient_ci
    participant S as eval-dispatch
    F->>H: POST push (signed)
    H->>H: verify signature, match repository
    H->>C: fan_out_triggers -> apply_trigger
    C->>C: dedup, concurrency
    C->>C: trigger_evaluation: Commit + Queued evaluation
    S->>S: woken by the new evaluation, hands it to a worker
```

## Routes

| Route | Verification |
|---|---|
| `POST /api/v1/hooks/github` | `X-Hub-Signature-256` against `GRADIENT_GITHUB_APP_WEBHOOK_SECRET_FILE`; `503` until the App is fully configured |
| `POST /api/v1/hooks/{forge}/{project}/{integration}` | `gitea` / `forgejo`: HMAC from `X-Forgejo-Signature`, then `X-Gitea-Signature`. `gitlab`: `X-Gitlab-Token` compared in constant time. `github` answers `400` |

- The generic route finds the integration by `(project, inbound, name)`, without `forge_type`: one inbound row serves all three forges.
- The secret is decrypted with the crypt file; `allowed_ips` rejects other sources with `403`.
- GitHub `installation` / `installation_repositories` events upsert or clear `github_installation` and seed the `github-<login>` integration pair (`github-<installation_id>` without a login).

## GitHub App Events

| Event | Handler |
|---|---|
| `push` | Push chain below |
| `pull_request` | Pull request triggers |
| `release` | `dispatch_github_app_release`: triggers with `releases_only` |
| `check_run` | `approval::handle_github_check_run` |
| `pull_request_review` | `approval::handle_pull_request_review` |
| `issue_comment` | `commands::handle_issue_comment`: comment commands |
| `installation`, `installation_repositories` | `handle_github_installation` |

## Push Chain

| Step | Code | Does |
|---|---|---|
| 1 | `fan_out_triggers` (`forge_hooks/fanout.rs`) | Active `task_trigger` rows of type `ReporterPush` whose branch and tag globs match |
| 2 | `normalize_repo_url`, `event_repo_matches_task` | Strips `.git` and a trailing `/`, rewrites `git@` URLs; compares lowercased `owner/repo`, ignoring the host |
| 3 | `gradient_ci::apply_trigger` | Dedup and concurrency: aborts the running evaluation or parks the new one in `Waiting`; a violation of `uq_evaluation_one_active_per_task` maps to `SkippedConcurrency` |
| 4 | `gradient_ci::trigger::trigger_evaluation` | Inserts the `Commit` row and a `Queued` evaluation, sets `task.force_evaluation`, resets `last_check_at` |
| 5 | `eval-dispatch` | Woken by the created evaluation (`record_evaluation_created`), offers it to workers; the 5 s tick is the fallback |

## Related

- [Connect GitHub](../../guides/forge-github.md): the setup side
- [Events and Webhooks](../../reference/events.md): outbound events
