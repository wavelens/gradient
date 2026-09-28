# Internals

Implementation details that fit no other contributor page: forge hooks, NAR storage and signing, the graph API, recursive SQL walks, authentication and the deep GC. Paths are relative to `backend/`.

| Topic | Page |
|---|---|
| Evaluation steps, fetch and walk | [Jobs](proto/jobs.md), [Eval Worker Setup](eval-worker.md) |
| Batch ingest, anchors | [Build Anchors](scheduler/build-anchors.md) |
| Promotion, dispatch gates, failure cascade | [Promotion and Counters](scheduler/promotion-and-counters.md) |
| Offers, assignment, scoring | [Capabilities and Dispatch](proto/capabilities-and-dispatch.md), [Scoring](scheduler/scoring.md) |
| Uploads, downloads | [Transfer](proto/transfer.md) |
| Wholeness, runtime ripple, cache access | [Cache Closure](scheduler/cache-closure.md) |
| Worker registration and auth | [Connection](proto/connection.md) |
| Statuses | [Evaluations and Builds](../concepts/evaluations-and-builds.md) |

## Forge Webhooks

`gradient-web/src/endpoints/forge_hooks/` receives forge events. The routes carry no session; each delivery proves itself with the forge's signature.

| Route | Verification |
|---|---|
| `POST /api/v1/hooks/github` | `X-Hub-Signature-256` against `GRADIENT_GITHUB_APP_WEBHOOK_SECRET_FILE`; `503` until the App is fully configured |
| `POST /api/v1/hooks/{forge}/{project}/{integration}` | `{forge}` is `gitea`, `forgejo` or `gitlab` (`github` answers `400`). Gitea/Forgejo HMAC (`X-Forgejo-Signature`, then `X-Gitea-Signature`); GitLab compares `X-Gitlab-Token` in constant time |

- The generic route looks the integration up by `(project, inbound, name)`, without `forge_type`: one inbound row serves all three forges. The secret is decrypted with the crypt file; `allowed_ips` rejects other sources with `403`.
- GitHub `installation` / `installation_repositories` events upsert or clear `github_installation` and seed the `github-<login>` integration pair (`github-<installation_id>` without a login).
- Handled events: `push`, `pull_request`, `release`, `check_run`, `issue_comment`, `pull_request_review`.

**Push chain:**

1. `trigger_push_for_integration` -> `fan_out_triggers` (`forge_hooks/fanout.rs`): active `task_trigger` rows of type `ReporterPush` whose branch and tag globs match.
2. Repository match: `normalize_repo_url` strips `.git` and a trailing `/` and rewrites `git@` URLs; `event_repo_matches_task` compares lowercased `owner/repo`, ignoring the host.
3. `gradient_ci::apply_trigger` applies dedup and concurrency: a running evaluation is aborted, or the new one parks in `Waiting`; a violation of `uq_evaluation_one_active_per_task` maps to `SkippedConcurrency`.
4. `gradient_ci::trigger::trigger_evaluation` inserts the `Commit` row and a `Queued` evaluation, sets `task.force_evaluation` and resets `last_check_at`.
5. The `eval-dispatch` tick (every 5 s) hands the evaluation to a worker.

## NAR Storage

- **Layout:** `${baseDir}/nars/<2 chars>/<rest>.nar.zst`, keyed by the **store-path hash**: a presigned upload URL exists before the worker knows the content hash. The narinfo advertises `nar/<file_hash>.nar.zst`; `resolve_effective_hash_db` maps the file hash back.
- **Idempotent REST uploads:** `ingest_nar_reader` (`nix copy` push) goes through `put_nar_idempotent`, which skips the write when `cached_path` records the same `file_hash` and a `HEAD` finds the object.
- **Proto commits** verify length and SHA-256 and move the relayed file in with `adopt_file`; presigned uploads bypass the server.
- **Local storage** relays every upload through the server.
- `last_fetched_at` and `fetch_count` feed eviction.

!!! warning "Bucket Requirements"
    - No object versioning (nor object lock or replication, which force versioning): Gradient assumes overwrite on PUT, and a versioned bucket keeps one copy per re-upload that no S3 GC reclaims.
    - An `AbortIncompleteMultipartUpload` lifecycle rule (e.g. 7 days): NARs over 1 GiB go up as multipart, and a worker that dies mid-upload leaves the parts behind.

## Signing and Narinfo

- **Keys:** one Ed25519 key per cache, encrypted with the crypt secret. `format_cache_key` returns the decrypted private key; `format_cache_public_key` the `<host>-<name>:<base64>` public key.
- **The server signs**, never the worker: `gradient_proto::signing::sign_cached_path`, called on a NAR commit and on a REST upload. A commit queues `cached_path_signature` placeholders for every subscribed cache; the sign sweep (`gradient-cache/src/cacher/sign_sweep.rs`, `sign_missing_signatures`) fills them. Subscribing a cache later inserts placeholders too.
- **Narinfo** (`GET /cache/{cache}/<hash>.narinfo`) is built from the database only: `derivation_output`, `cached_path`, `cached_path_signature`, with `CA:` for content-addressed paths. A hash without a `derivation_output` falls back to `cached_path` (`.drv` files, standalone paths). The server never re-packs or re-hashes a NAR.
- **Pull-through:** an upstream narinfo gets `URL:` rewritten to `nar/upstream/{id}/...` and is re-signed only after the upstream signature verifies; `X-Cache` tells local from upstream.

## Debug Info

`GET /cache/{cache}/debuginfo/{build_id}` mirrors Nix's `index-debug-info`:

```json
{"archive": "../nar/<file_hash>.nar.zst", "member": "lib/debug/.build-id/<xx>/<yy>.debug"}
```

- `archive` is relative to the requested key. Both spellings are accepted: `<build-id>` (Hydra) and `<build-id>.debug` (`nix copy`). The `cached_path_signature` join is the access gate, as for narinfo.
- The `debug_info` index comes from walking the NARs of paths ending in `-debug` (`separateDebugInfo` outputs). An upload walks its own NAR on a detached task; `cached_path.debug_info_indexed` marks a scanned NAR, and the `debug-index` sweep reads each `file_hash` at most once.
- A miss falls through to the upstreams and rewrites `archive` through `nar/upstream/{id}/...`. An `archive` that is absolute or escapes the upstream root is refused.

**Status codes under `/cache/{cache}/`:** an unknown key answers `404`, never another `4xx`: substituters and debuginfod clients treat other codes as hard errors instead of trying the next source. A disabled cache answers `400`, a private cache without credentials `401`.

## Closure Rows

`cache_derivation(cache, derivation)` exists when every output of the derivation is cached and every dependency has its own row for the same cache, for projects subscribed to the cache.

- Written by the sign sweep's `record_newly_completed_derivations`, with a full backfill at most once an hour.
- Revoked by `gradient_db::revoke_cache_closures`.
- Nothing reads the table today.

## Dependency Graph API

`GET /builds/{build}/graph` (`gradient-web/src/endpoints/builds/graph.rs`) walks `derivation_dependency` breadth-first from the build and maps each derivation to its `build_job` in the same evaluation.

- The frontier is a set of `BuildJobId`s; a dependency without a `build_job` in the evaluation is dropped.
- A soft cap of 500 nodes, checked before each wave.
- About five queries per wave: the query count follows the depth, not the node count.
- No `kind` filter: runtime edges appear too.
- `DependencyEdge { source, target }`: `source` is the dependency, built before `target`.
- `GET /builds/{build}/dependencies` lists the direct dependencies.

## Recursive Graph Walks

`gradient-db/src/graph_sql.rs` generates the build closure, the failure cascade, the runtime closure (`runtime_closure_cte`, `kind IN (1, 2)`) and the GC keep-set (`live_cached_paths_cte`). Callers pass a seed and a direction and get a `WITH RECURSIVE` prelude; walks run under `begin_walk`, which sets `work_mem = '64MB'`.

Hand-written walks outside the module: `walk_completeness.rs`, `runtime_readiness.rs` (`recount_sql`), `task_board.rs` (`DEP_COUNTS_SQL`), `cache_storage.rs` (`revoke_cache_closures_sql`).

### The `OFFSET 0` Fence

`graph_sql.rs` (`lateral_step`) and `walk_completeness.rs` put the recursive term behind a `LATERAL` probe and an `OFFSET 0` fence:

```sql
WITH RECURSIVE closure(derivation) AS (
    SELECT unnest($1::uuid[])
  UNION
    SELECT s.next FROM closure c, LATERAL (
      SELECT e.dependency AS next FROM derivation_dependency e
      WHERE e.derivation = c.derivation OFFSET 0) s)
```

- Postgres estimates a recursive working table at ten times the seed: 348 870 rows estimated against 2 439 actual on a 44 000-node closure.
- At that estimate a merge join over the whole edge index looks cheaper than a nested loop, and the planner rescans four million edges per iteration.
- `OFFSET 0` stops the pull-up; a correlated lateral can only run as a nested loop with an index lookup per row.

| Walk (production) | Plain join | Fenced |
|---|---|---|
| Evaluation closure, 43 898 nodes | 5 278 ms | 955 ms |
| GC keep-set, 315 155 nodes | 40 069 ms | 9 746 ms |

The set operator stays `UNION`, which deduplicates the frontier each iteration. The dependents walk emits 940 000 rows for 68 000 distinct nodes; `UNION ALL` grows exponentially with depth on diamond graphs.

### Indexes and Ripples

- `derivation_dependency` has no surrogate key: the pair is the primary key (`derivation_dependency_pkey`), with `idx-derivation_dependency-reverse-pair` for the other direction. Both are covering: walks are index-only.
- `idx-derivation_dependency-runtime`, a partial `(dependency) INCLUDE (derivation) WHERE kind IN (1, 2)`, drives the wholeness ripple from a dependency to the anchors counting it.
- A ripple level is three statements: `RUNTIME_DEPENDENT_COUNTS` reads the dependents and their edge counts, `lock_anchors` locks them in `derivation` order, and `COUNT_DOWN_RUNTIME` / `COUNT_UP_RUNTIME` move the counters through `unnest` of the bound set.
    - Deriving the set inside the update left the planner without a small driver: a sequential scan of `cached_path` that took row locks in physical order and deadlocked NAR commits against maintenance about every six minutes.
- The GC freshness seed reads `build_job` and `entry_point` by `created_at` (`idx-build_job-created_at`, `idx-entry_point-created_at`, both `INCLUDE (derivation)`). The cutoff is the candidate scan's start: the seed matches almost nothing and must not scan to find that out.

### Instance Metrics

Every 30 s (`GRADIENT_METRICS_INSTANCE_INTERVAL_SECS`) the instance pass averages nine `derivation_metric` values and four `dispatched_job` values over 5 min, 1 h and 24 h windows.

- `missing_nar_size`, `missing_count` and `dependency_count` are columns on `dispatched_job`, carried by `idx-dispatched_job-build-window` next to `ready_at`: the aggregate is an index-only scan.
- Reading them out of the `job_context` jsonb measured 1.94M buffers and 1.6 s for 449k rows in production.
- The columns are not backfilled: `AVG` skips nulls as it skipped a missing JSON key, and no window exceeds a day.

### SQL/PGQ

PostgreSQL 19's SQL/PGQ (`GRAPH_TABLE`) is not used: the first implementation matches fixed-length patterns only, and every walk here has unbounded depth. `derivation` and `derivation_dependency` already have the vertex and edge table shape `CREATE PROPERTY GRAPH` needs; a switch would touch `graph_sql.rs` and the hand-written walks above. Revisit when quantified path patterns land.

## Authentication

| Token | Details |
|---|---|
| Session JWT | HS256 with `GRADIENT_SECRETS_JWT_FILE`. Claims `{ exp, iat, id, jti }`, `jti` the `session` row. 24 h, or 30 days with `remember_me`. Minted by `create_session_and_token` |
| API key | 64 random alphanumeric characters, stored as SHA-256 hex, returned with a `GRAD` prefix. Carries `expires_at`, `revoked_at`, an optional project or cache pin, a permission mask and `allowed_ips` |
| Download token | 1 h, from `encode_download_token` |

- Tokens come from `Authorization: Bearer` or the `jwt_token` cookie.
- Each request checks the session row for revocation and expiry.
- `api.last_used_at` and `session.last_used_at` are stamped at most once a minute (`LAST_USED_STAMP_INTERVAL`); a failed stamp is logged, never fatal.
- **OIDC:** `oidc_login_create` builds the authorization URL with PKCE (S256) and keeps `state`, `nonce` and the verifier in a signed `oidc_csrf` cookie (10 min). `oidc_login_verify` checks `state`, exchanges the code, verifies the ID token against the provider JWKS and upserts the user; the endpoint then mints the session. Discovery reads `<discoveryUrl>/.well-known/openid-configuration`.

## Deep GC

Long-running admin operations live in `admin_task`: `kind` (`deep_gc`) and `status` (`pending` -> `running` -> `completed` / `failed`). The partial unique index `admin_task_one_active_per_kind` allows one active task per kind; a second `POST` answers `409`.

`POST /api/v1/admin/maintenance/deep-gc` (superuser, `202`) inserts the row and starts the sweep via `Shutdown::spawn`. Three passes reconcile storage against the database (`gradient-cache/src/cacher/deep_gc.rs`):

| Pass | Removes |
|---|---|
| NAR | `cleanup_orphaned_cache_files`: objects without `cached_path` rows and rows without objects. Evicting stale live paths is maintenance's job |
| Blob | `build-request-blobs/...` objects and `build_request_blob` rows without a partner |
| Log | Logs keyed by `BuildAttemptId` without a `build_attempt` row; an attempt without a log is legitimate |

- Progress is flushed to `admin_task.progress` between passes and read at `GET /api/v1/admin/tasks[/{task_id}]`.
- A failed pass stops the sweep and keeps the partial report.
- On restart, every non-terminal task is marked `failed` ("server restarted before completion") before the web layer serves; each pass is idempotent, and a new `POST` starts over.
