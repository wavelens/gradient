# Tests

This page tracks notable tests added to Gradient and where they live.

## Proto handshake — organization peer filtering

Helper `filter_org_peers_without_cache` runs during the `/proto` handshake's
`perform_auth` step. After token validation, each authorized peer that is an
organization is checked against the `organization_cache` table. Organizations
without a subscribed cache are moved into `failed_peers` with reason
`"organization has no cache subscribed"`. If the authorized peer set ends up
empty the connection is rejected with `401 no valid peer tokens provided`.

Backend tests (in `backend/core/src/proto/handler/auth.rs`):

- `proto::handler::auth::tests::filter_org_peers_passes_through_org_with_cache`
- `proto::handler::auth::tests::filter_org_peers_demotes_org_without_cache`
- `proto::handler::auth::tests::filter_org_peers_passes_through_non_org_uuids`
- `proto::handler::auth::tests::filter_org_peers_mixed`
- `proto::handler::auth::tests::validate_then_filter_demotes_org_without_cache`

## Frontend — workers page no-cache banner

When the active organization has no subscribed cache, the workers page shows
a banner instructing the admin to subscribe to a cache before workers can run.

- `WorkersComponent — no-cache banner` — banner show/hide specs at
  `frontend/src/app/features/organizations/workers/workers.component.spec.ts`

## Inbound forge webhook response-body (BaseResponse envelope)

Integration tests in `backend/web/tests/forge_hooks.rs` verify that both
webhook endpoints (`POST /api/v1/hooks/{forge}/{org}/{name}` and
`POST /api/v1/hooks/github`) return a correctly-shaped
`BaseResponse<WebhookResponse>` envelope under all common scenarios.

Run with: `cargo test -p web --test forge_hooks`

Tests covered:

| # | Test name | Scenario |
|---|-----------|----------|
| 1 | `forge_webhook_no_matching_project` | Gitea push, valid signature, no active project tracks the repo → 200, `projects_scanned=0`, empty `queued`/`skipped`. |
| 2 | `forge_webhook_matching_project_queues` | Gitea push, valid signature, one matching project → 200, one item in `queued` with correct `project_name` and `organization`. |
| 3 | `forge_webhook_invalid_signature` | Gitea push, wrong HMAC → 401, `error=true`, `message="invalid webhook signature"`. |
| 4 | `forge_webhook_integration_not_found` | Org found but integration row absent → 404, `message="integration not found"`. |
| 5 | `github_app_webhook_push_queues` | GitHub App push, valid `X-Hub-Signature-256`, one matching project → 200, one item in `queued`. |
| 6 | `github_app_webhook_ping` | GitHub App ping event → 200, `event="ping"`, all arrays empty. |
| 7 | `github_app_webhook_installation` | GitHub App installation event, org not found in DB (warns, does not error) → 200, `event="installation"`, empty queued. |
| 8 | `github_app_webhook_not_configured` | GitHub App config absent (`github_app_webhook_secret_file=None`) → 503, `message="github app integration not configured"`. |

**Deferred (Task 8):**

The following scenarios are intentionally omitted because they would duplicate
`trigger_evaluation` unit tests already present in `backend/core/src/ci/trigger.rs`:

- *already_in_progress*: project has an in-progress eval → item appears in `skipped` with `reason="already_in_progress"`.
- *no_previous_evaluation*: `trigger_restart_builds` finds no previous eval → `reason="no_previous_evaluation"`.
- *db_error during trigger*: DB returns an error inside the per-project loop → `reason="db_error"`.

These can be added as further `forge_hooks.rs` tests by extending the
`MockDatabase` chain to return an in-progress evaluation row (or error) instead
of the empty list at the in-progress-eval query position.

## GitHub App manifest flow

Backend (`cargo test -p core --tests ci::github_app_manifest`):
- `build_manifest_strips_trailing_slash`
- `build_manifest_uses_serve_url_in_all_url_fields`
- `build_manifest_has_default_permissions_and_events`
- `manifest_post_url_github_com`
- `manifest_post_url_enterprise_host`
- `api_base_url_github_com`
- `api_base_url_enterprise`
- `exchange_code_happy_path`
- `exchange_code_non_2xx_errors`

Backend (`cargo test -p core --tests ci::manifest_state`):
- `issue_state_returns_unique_tokens`
- `validate_and_consume_succeeds_then_fails_on_replay`
- `validate_and_consume_unknown_state_fails`
- `issue_state_prunes_expired_entries`
- `store_and_take_credentials_one_shot`

Backend (`cargo test -p web --tests endpoints::admin::github_app`):
- `validate_host_accepts_normal_hosts`
- `validate_host_rejects_path_chars`
- `validate_host_rejects_empty`

Frontend (`pnpm --dir frontend exec ng test --include='**/github-app.component.spec.ts' --watch=false`):
- `shows the setup view when ready=1 is absent`
- `clicking create-button calls requestGithubAppManifest with host`
- `renders credentials when ready=1 and the API returns them`

## Narinfo Deriver field

Backend (`cargo test -p web --tests narinfo`):
- `narinfo_served_from_db_without_daemon_probe` — verifies the `.narinfo`
  response is assembled from DB rows (no nix-daemon probe) and now also asserts
  that the optional `Deriver:` line is emitted when `cached_path.deriver` is
  populated. Worker-supplied deriver metadata arrives via `NarUploaded.deriver`
  and is persisted in `mark_nar_stored`.
- `shows a friendly error when credentials are no longer available`

## Upstream narinfo metadata for worker prefetch

Backend (`cargo test -p proto --lib handler::cache::tests`):
- `parse_upstream_narinfo_full_fields` — verifies the server parses
  `NarHash`, `NarSize`, `FileSize`, `References`, `Deriver`, and `Sig` from an
  upstream `.narinfo` body so the worker receives enough metadata to build a
  `ValidPathInfo` and call `add_to_store_nar`. Without this the worker
  silently failed imports and the build died with
  "dependency does not exist, and substitution is disabled".
- `parse_upstream_narinfo_requires_url` — a narinfo without `URL:` is rejected.
- `parse_upstream_narinfo_trims_base_url_trailing_slash` — joins
  `base_url` + `URL:` without double slashes.
- `parse_upstream_narinfo_empty_references_is_some_empty` — `References:` with
  no paths yields `Some(vec![])`, not `None`.
- `parse_upstream_narinfo_ignores_unparseable_sizes` — malformed `NarSize` /
  `FileSize` fall back to `None` rather than aborting the parse.

## Worker prefetch robustness — uncached inputs and broken daemon connections

Backend (`cargo test -p worker --tests`):
- `nix::store::tests::remote_errors_are_recoverable` — `is_connection_corrupt`
  returns `false` for daemon-side `Remote` errors (e.g. "build failed"); those
  leave the protocol stream aligned and the pooled connection is safe to
  reuse.
- `nix::store::tests::io_errors_mark_connection_corrupt` — IO-level daemon
  errors are flagged corrupt; without this a desynced pooled connection gets
  handed to the next caller and surfaces as confusing downstream parse
  errors (`parse error L, non-absolute store path "L"`).
- `nix::store::tests::custom_errors_are_treated_as_corrupt` — opaque `Custom`
  errors are conservatively flagged corrupt: we can't tell a framing bug
  from anything else, so the connection is dropped.
- `proto::nar_import::tests::classify_splits_cached_by_url_presence` — cached
  entries with a presigned `download_url` go to the S3 bucket, those without
  go to the WebSocket `NarRequest` bucket.
- `proto::nar_import::tests::classify_collects_uncached_separately` —
  regression guard for the Stage-3 prefetch hard-fail: when the server
  reports a required input as `Uncached`, it is *not* silently skipped.
  Previously the path was dropped on the floor and a dependent build
  eventually failed inside `add_to_store_nar` with
  `path '/nix/store/…' is not valid`; classifying it explicitly lets the
  prefetcher abort with a clear message that names the missing path.
- `proto::nar_import::tests::classify_empty_input_is_empty_output` — empty
  cache responses produce empty buckets.

## State configuration — optional fields for OIDC-only users

Backend (`cargo test -p core --lib state::tests`):
- `user_accepts_missing_password_file` — `StateUser` accepts a JSON
  document with `"password_file": null`, so the NixOS module may emit
  OIDC-only users without a password credential file.
- `org_project_cache_descriptions_optional` — `description` on
  organizations, projects, and caches is optional; a full config without
  them validates cleanly.

These pin the wire contract between `nix/modules/gradient-state.nix`
(`types.nullOr types.str` on `password_file` and the three `description`
options) and `backend/core/src/state/mod.rs`. Without them, provisioning a
user intended for OIDC failed at startup with "missing field
`password_file`", and the user's subsequent OIDC login was rejected by
`web::authorization::oidc` with `User already exists with password
authentication`.

## Build → worker attribution

The `build.worker` column (text, nullable) records which worker executed a
build. It replaces the dead `build.server` Uuid left over from the SSH
build-machine era. `Scheduler::handle_build_status_update` writes the
connected worker's `worker_id` (the identity it sent in `InitConnection`)
the first time it reports `Building` for a given build, alongside the
existing state-machine transition. The value surfaces via
`GET /api/v1/builds/{build}` as the `worker` field of `BuildWithOutputs`.

No dedicated scheduler test was added: the transition path is already
covered by the compile-checked MockDatabase fixtures in
`scheduler/src/handler_tests.rs`, and the one-line UPDATE added in
`handle_build_status_update` is a trivial write that reuses the existing
`ABuild::update` path. The migration and entity alignment are verified by
`cargo test --workspace --tests` — any `entity::build::Model` literal
that forgot the new field would fail to compile.

## EvalMessage — worker-surfaced evaluation messages

Backend (`cargo test -p scheduler --tests scheduler_tests::record_eval_message`):
- `record_eval_message_drops_when_job_unknown` — a `ClientMessage::EvalMessage`
  whose `job_id` is not an active scheduler job is silently accepted (no DB
  insert, no error). Ensures stale messages from finished jobs can't poison
  the evaluation log.
- `record_eval_message_inserts_for_active_build_job` — for an enqueued build
  job the handler resolves `PendingJob::evaluation_id()` and inserts one row
  into `evaluation_message`. Build compile failures and user-initiated aborts
  deliberately do not flow through this path.
