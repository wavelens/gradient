# Tests

This page tracks notable tests added to Gradient and where they live.

## OIDC — CSRF cookie, ID-token verification, identity binding

Tests in `backend/web/src/authorization/oidc.rs` cover the security
fixes for issue #38:

- `random_url_safe_is_unique_and_url_safe` — `state`/`nonce` are
  cryptographically random and URL-safe.
- `csrf_cookie_roundtrips` / `csrf_cookie_rejects_wrong_secret` /
  `csrf_cookie_rejects_expired` — the `oidc_csrf` cookie is an
  HMAC-signed JWT that round-trips, fails verification under a
  different secret, and is rejected when expired.
- `state_compare_constant_time_rejects_mismatch` — `state` comparison
  uses `subtle::ConstantTimeEq`.

The full ID-token verification path (signature against the provider's
JWKS, `iss`/`aud`/`exp`/`nonce` checks, identity bound to
`(oidc_issuer, oidc_subject)` rather than email) is enforced in
`oidc_login_verify` and exercised end-to-end via the
`/auth/oauth/authorize` and `/auth/oidc/callback` endpoints.
## Unified resource access — `crate::access` and `crate::permissions`

All "load resource by name and check the caller may use it" logic lives in
two modules:

- `backend/web/src/permissions.rs` — declares the [`Permission`] capability
  enum (e.g. `EditProject`, `ManageMembers`, `ManageWebhooks`) and the
  `role_grants(role_id, permission)` lookup. Today the role → permission
  mapping is hardcoded for the three built-in roles; replacing this single
  function with a DB-driven lookup is what enables custom roles configurable
  in the frontend.
- `backend/web/src/access.rs` — exposes `load_org`, `load_project`,
  `load_cache`, `load_webhook_in_org`, `load_integration_in_org`, plus the
  predicates `is_org_member` / `has_permission`. Each loader takes an access
  policy enum (`OrgAccess`, `ProjectAccess`, `CacheAccess`) so handlers
  declare *what level of access they need* rather than stitching together
  ad-hoc lookup + permission + state-managed checks.

Unit tests in `access.rs` cover the role matrix and the managed-resource
guard:

- `org_admin_passes` — admin role + permission grants the resource.
- `org_admin_view_role_forbidden` — view role + admin-required permission →
  `WebError::Forbidden`.
- `org_admin_managed_forbidden` — state-managed org rejected for mutating
  permissions.
- `org_admin_non_member_not_found` — non-member → `WebError::NotFound`
  (no leak between "missing" and "not a member").
- `org_writable_write_role_passes` / `org_writable_view_role_forbidden` —
  write-tier permission honors Admin+Write but rejects View.
- `org_member_view_role_passes` — `OrgAccess::Member` accepts any role.
- `org_readable_public_visible_to_anon` /
  `org_readable_private_invisible_to_anon` — visibility rule for anonymous
  callers.
- `project_editable_admin_passes` / `project_editable_view_forbidden` /
  `project_editable_managed_forbidden` / `project_missing_returns_project_label` —
  same matrix at the project level, including the project-existence label
  guarantee.

Unit tests in `permissions.rs` lock the role → permission mapping itself:

- `admin_grants_everything`
- `write_excludes_admin_only`
- `view_cannot_edit_projects_or_webhooks`
- `unknown_role_grants_nothing`
- `view_org_is_not_mutating`

Run with: `cargo test -p web --lib access::tests`
and `cargo test -p web --lib permissions::tests`.

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

## Auth middleware response envelope

Integration tests in `backend/web/tests/auth_middleware.rs` lock in the
HTTP-status + `BaseResponse<String>` body returned by the `authorize`
middleware after it was rewritten to return `WebError` instead of building
the envelope by hand (issue #55).

Run with: `cargo test -p web --test auth_middleware`

| Test | Scenario | Expected |
|------|----------|----------|
| `missing_auth_header_returns_403_envelope` | request to a protected route with no `Authorization` header and no cookie | 403, `error=true`, `message="Authorization header not found"` |
| `malformed_bearer_returns_403_envelope` | `Authorization` header present but not `Bearer <token>` | 403, `message="Invalid Authorization header"` |
| `undecodable_token_returns_401_envelope` | `Bearer` token that JWT can't decode | 401, `message="Unable to decode token"` |

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

Backend (`cargo test -p core --tests ci::reporting`):
- `maps_terminal_states` — `EvaluationStatus::{Completed, Failed, Aborted}` map to `CiStatus::{Success, Failure, Error}`.
- `skips_intermediate_states` — non-terminal statuses produce no CI status (avoids double-reporting `Running`).

Backend (`cargo test -p core --tests ci::manifest_state`):
- `issue_state_returns_unique_tokens`
- `validate_and_consume_returns_user_then_fails_on_replay`
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

## Per-project `sign_cache` option (#125)

Backend:

- `cache::cacher::sign_sweep::tests::skip_when_all_producing_projects_private` —
  `compute_skipped_cached_paths` skips a path iff every producing project has
  `sign_cache=false` and at least one such project exists. A mixed
  public+private path stays signed (option B semantics).
- `cache::cacher::sign_sweep::tests::skip_set_empty_when_no_private_producers` —
  no skips when all producers are public.
- `web::tests::projects_sign_cache::get_project_includes_sign_cache` — GET
  `/api/v1/projects/{org}/{name}` returns `sign_cache` in the response body.
- `web::tests::projects_sign_cache::patch_project_writes_sign_cache_false` —
  PATCH with `{ "sign_cache": false }` is accepted and round-trips.
- `web::tests::projects_sign_cache::create_project_accepts_sign_cache_false` —
  PUT body may include `sign_cache: false`; default is `true` when omitted.
- `web::tests::narinfo::narinfo_returns_404_when_signature_null` —
  regression: when `cached_path_signature.signature` is NULL (the state the
  sweep leaves rows in for `sign_cache=false` projects), the narinfo handler
  returns 404 rather than serving an unsigned narinfo. The whole privacy
  guarantee depends on this and we lock it in here.

## `mark_nar_stored` filters derivation_output by hash

`proto::handler::nar::mark_nar_stored` previously located the
`derivation_output` row to mark cached by filtering on the full `output`
string. That field can drift from the worker's `store_path` argument
(e.g. when an output's `path` was empty at eval time and the row was
filtered out, or when a derivation came back as a "known/pruned" subtree
with no outputs persisted), causing `is_cached` to silently stay `false`
and every subsequent narinfo lookup for the build output to 404.

The filter now uses `hash` — the same column the read path
(`get_nar_by_hash`) filters on — and updates **all** matching
`derivation_output` rows, also linking each row's `cached_path` UUID
to the freshly-written `cached_path` row.

## Worker NAR upload — store path normalisation

Eval-worker `get_derivation_path` returns drv paths as bare `<hash>-<name>.drv`
strings (no `/nix/store/` prefix). `nar::push_direct` and `nar::upload_presigned`
must canonicalise to the absolute path before handing it to harmonia's
`NarByteStream` (otherwise the NAR is empty → `NarSize: 0`) and before sending
`NarUploaded.store_path` to the server (otherwise `cached_path.store_path` is
stored without prefix and the served narinfo `StorePath:` line is malformed).

Backend (`cargo test -p worker --bins proto::nar::tests::ensure_full_store_path`):
- `ensure_full_store_path_prefixes_bare_hash_name` — bare drv path gets
  `/nix/store/` prepended.
- `ensure_full_store_path_preserves_absolute` — `/nix/store/...` paths are
  passed through unchanged.
- `ensure_full_store_path_preserves_other_absolute_paths` — unrelated absolute
  paths (e.g. test tmpdirs) are not touched.

## Hash column normalization (file_hash / nar_hash)

The `derivation_output.file_hash`, `cached_path.file_hash`, and
`cached_path.nar_hash` columns are persisted in the canonical `sha256:<nix32>`
form so the URL hash extracted from a narinfo `URL:` field matches the column
directly. Workers send `sha256:<hex>` over the wire; the proto handler and
scheduler call `gradient_core::nix_hash::normalize_nar_hash` before
`Set(...)`. Migration `m20260430_000000_normalize_hash_columns` backfills
pre-existing rows.

Backend:
- `cargo test -p core --lib nix_hash` — round-trip and idempotency tests for
  `normalize_nar_hash` covering SRI, prefixed hex, prefixed nix32, bare hex,
  and rejection of malformed inputs.
- `cargo test -p migration --lib normalize_hash_columns` — covers the
  hex→nix32 conversion helper used by the backfill migration.
- `cargo test -p web --lib endpoints::caches::nar::tests` —
  `resolve_returns_store_hash_for_normalized_derivation_output` is the
  regression test for the original 404 bug: a narinfo URL hash (nix32)
  resolves a `derivation_output` row whose `file_hash` is in canonical
  `sha256:<nix32>` form.

## NAR path extraction — file or directory subtree

`core::storage::nar_extract::extract_path_from_nar_bytes` returns either
`Extracted::File` (regular file body) or `Extracted::Directory { tar_zst }`
(zstd-compressed tar of the matched subtree). The download endpoints
(`/builds/{build}/download/{filename}` and the project-level entry-point
download) detect the variant and set `Content-Type: application/zstd` plus a
`.tar.zst`-suffixed `Content-Disposition` filename for the directory case.

Backend (`cargo test -p core --test nar_extract`):
- `extracts_file_at_relative_path`, `extracts_file_in_nested_directory`,
  `drains_non_matching_sibling_before_extracting_target`,
  `returns_not_found_for_missing_path` — file-mode behaviours preserved.
- `extracts_directory_as_tar_zst` — regression for "fails if build output is
  a folder": when the build product's relative path resolves to a directory
  in the NAR, the extractor walks the subtree, emits tar entries for nested
  directories and files (preserving the executable bit), and zstd-compresses
  the result.
- `directory_tarball_preserves_symlinks` — symlinks inside the matched
  subtree are written as `tar::EntryType::Symlink` with the original target
  bytes, not flattened to regular files.
- `directory_match_at_root_via_basename` — a build product whose path equals
  the output store path returns the whole subtree as `tar.zst`, with entries
  rooted at the matched directory's basename so extraction recreates that
  name.

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
- `state_project_accepts_wildcard_field` — `StateProject` deserialises
  the canonical `wildcard` field.
- `state_project_accepts_legacy_evaluation_wildcard_alias` — pre-rename
  state files using `evaluation_wildcard` continue to parse via the
  serde alias, so existing `gradient-state.nix` configurations don't
  break on upgrade.

These pin the wire contract between `nix/modules/gradient-state.nix`
(`types.nullOr types.str` on `password_file` and the three `description`
options) and `backend/core/src/state/mod.rs`. Without them, provisioning a
user intended for OIDC failed at startup with "missing field
`password_file`", and the user's subsequent OIDC login was rejected by
`web::authorization::oidc` with `User already exists with password
authentication`.

## Hashed API keys at rest

Backend (`cargo test -p core --lib state::provisioning::api_key_hash_tests`):
- `accepts_64_char_hex` — a lowercase 64-char hex string round-trips.
- `trims_trailing_whitespace` — credential files written with a trailing
  newline still parse.
- `lowercases_uppercase_hex` — uppercase hex is normalised on the way in.
- `rejects_plaintext_token` / `rejects_short_hex` / `rejects_non_hex_chars` —
  malformed values are rejected with a "SHA-256" hint pointing at the right
  shell incantation.

Backend (`cargo test -p core --lib state::provisioning::helper_tests`):
- `lookup_id_returns_id_when_present` / `lookup_id_errors_with_kind_and_name` —
  pin the shared `lookup_id` helper used by every `apply_*` provisioning step
  so missing user/org references produce a uniform `"<Kind> '<name>' not
  found"` error.
- `read_credential_default_dir_when_env_unset` /
  `credentials_dir_returns_nonempty` — pin the shared credential-file
  resolver: the error always names the file it tried to read and the file
  label, so an operator who misnames a systemd credential sees which one.

Backend (`cargo test -p migration --lib m20260502_000000_hash_api_keys`):
- `sha256_hex_known_vector` — pins the in-place migration's digest helper to
  `SHA-256("abc")`.

Together these guard the contract that every value in `api.key` is a
lowercase 64-char SHA-256 hex digest of the bearer token, and that
`gradient` rejects state credentials that don't match (so an operator who
accidentally points `key_file` at a plaintext token sees the error at
provisioning rather than authenticating with a hash that nothing can
produce).

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

## Cache GC — orphan files keep predicate

`cleanup_orphaned_cache_files` (`backend/cache/src/cacher/cleanup.rs`) is the
safety-net pass that removes NAR files in `nar_storage` with no DB references.
Its keep predicate is build-status aware: a hash is retained when its
`derivation_output` has any `build` row whose status is **not** `Failed`,
`Aborted`, or `DependencyFailed`. This covers `Substituted` builds (where no
worker ever uploaded the NAR locally and `is_cached` may stay false) as well
as the upload race window where the NAR is on disk before
`derivation_output.is_cached=true` is flipped. NARs referenced only by a
fully-uploaded `cached_path` row (e.g. `.drv` files) are also kept.

Run with: `cargo test -p cache --lib cacher::cleanup`

- `cacher::cleanup::tests::keeps_active_drops_orphan` — file for an active
  build's hash survives; file with no DB references is removed.
- `cacher::cleanup::tests::keeps_cached_path_only` — a hash returned only by
  the `cached_path.file_hash IS NOT NULL` UNION branch is kept (covers `.drv`
  files that have no `derivation_output`).
- `cacher::cleanup::tests::drops_everything_when_no_keep` — empty keep set
  removes every on-disk NAR.

## Cache GC — TTL pass orphan guard

`cleanup_stale_cached_nars` (`backend/cache/src/cacher/cleanup.rs`) evicts
`cache_derivation` rows whose `last_fetched_at` is older than
`nar_ttl_hours`. Its SELECT now also requires
`NOT EXISTS (build b WHERE b.derivation = cd.derivation AND b.status NOT IN
(Failed, Aborted, DependencyFailed))`, so derivations still referenced by an
active evaluation/build never get their NAR evicted purely because no one
fetched it recently. This implements the design "old evals/builds removed by
`keep_evaluations` → derivation becomes orphan → kept for `nar_ttl_hours` →
evicted".

The SELECT additionally excludes derivations that own a fixed-output
`derivation_output` (`ca IS NOT NULL`). FOD NARs originate from external
sources (e.g. `fetchurl` tarballs) that may not be re-fetchable, so a
transient gap in build references must never delete the only cached copy.
FODs are reclaimed only by `gc_orphan_derivations` after the grace period
and zero remaining build references.

- `cacher::cleanup::tests::stale_nars_disabled_when_ttl_zero` — pass is a
  no-op when `nar_ttl_hours = 0`.
- `cacher::cleanup::tests::stale_nars_no_eligible_rows` — empty SELECT
  result leaves on-disk NARs untouched.
- `cacher::cleanup::tests::ttl_select_skips_fixed_output_derivations` —
  regression for #107: the TTL SELECT must keep its `derivation_output.ca
  IS NOT NULL` guard so FOD NARs are never evicted by the TTL pass.

## Frontend — form primitives & style guide

Reusable form primitives live under
`frontend/src/app/shared/components/form/` and consolidate the
label + input + error + dialog + message-banner patterns previously
duplicated across feature components. A `/styleguide` route at
`frontend/src/app/features/styleguide/` exercises every primitive and
serves as a living reference.

Specs (vitest + jsdom):

- `FormFieldComponent` — renders label/required marker; toggles
  `has-error` class on touched + invalid control.
  (`shared/components/form/form-field/form-field.component.spec.ts`)
- `FormErrorComponent` — hidden until touched; resolves default
  messages by error key; honours overrides; formats `minlength` with
  required length.
  (`shared/components/form/form-error/form-error.component.spec.ts`)
- `MessageBannerComponent` — applies `--type` modifier class; uses
  default icon per type; honours custom icon override.
  (`shared/components/form/message-banner/message-banner.component.spec.ts`)
- `PasswordInputComponent` — toggles input type between `password`
  and `text` on the eye button.
  (`shared/components/form/password-input/password-input.component.spec.ts`)
- `FormFieldsBuilder` — typed wrappers for text/email/password/confirm
  produce controls with the expected validators; password strength
  validator covers length + character class requirements; cross-field
  `confirm()` validates against the named control.
  (`shared/components/form/form-fields-builder.spec.ts`)

## CI check names — org/project context

CI check names reported to GitHub/Gitea now include the organization
and project so multiple Gradient instances/projects sharing a forge
repository remain distinguishable. Helpers live in
`backend/core/src/ci/reporting.rs` and are reused by the scheduler
(`backend/scheduler/src/ci.rs`) and the core reporters
(`backend/core/src/ci/reporting.rs`):

- Evaluation roll-up: `Gradient Evaluation {org}/{project}` (e.g.
  `Gradient Evaluation wavelens/my-project`).
- Per-entry-point build: `Gradient Build {org}/{project}: {entry_point}`.
- When the organization lookup returns `None`, the scope degrades to
  just `{project}`.

Tests (`cargo test -p core --tests ci::reporting`):

- `check_scope_with_org` — `Some("wavelens"), "my-project"` →
  `"wavelens/my-project"`.
- `check_scope_without_org_falls_back_to_project` — `None, "my-project"`
  → `"my-project"`.
- `evaluation_context_format` — produces the new
  `"Gradient Evaluation …"` string.
- `build_context_format` — produces
  `"Gradient Build wavelens/my-project: my-package"`.
- `build_context_falls_back_when_org_missing` — degrades correctly when
  the organization is unknown.

## Per-IP HTTP rate limiting

The web layer enforces per-client-IP token-bucket rate limits via
`tower_governor` (`backend/web/src/lib.rs`). Routes are grouped into four
fixed tiers (no CLI knobs):

| Tier | Routes | Refill | Burst |
|---|---|---|---|
| `auth` | `/api/v1/auth/{basic/login,basic/register,check-username,verify-email,resend-verification,oauth/authorize,oidc/login,oidc/callback}` | 1 req / 6 s | 5 |
| `webhook` | `/api/v1/hooks/...` | 1 req / s | 30 |
| `cache` | `/cache/{cache}/...` (public NAR surface) | 1 req / 60 ms | 1000 |
| `default` | everything else under `/api/v1` and `/proto` | 1 req / 200 ms | 150 |

Client IP is extracted from `X-Forwarded-For` / `X-Real-IP` (deployments
are expected behind a reverse proxy), falling back to `ConnectInfo`,
falling back to a single global bucket if no signal is available
(prevents 500s in tests / direct hits).

Tests (`cargo test -p web --test rate_limit`):

- `auth_tier_throttles_burst` — 5 successive `POST /api/v1/auth/check-username`
  requests succeed, 6th returns `429`.
- `cache_tier_does_not_throttle_moderate_burst` — 50 successive GETs to
  `/cache/{cache}/nix-cache-info` never return `429`.

## Direct-build multipart upload — filename validation

`POST /api/v1/builds` accepts file uploads via standard multipart parts
with field name `files`; the upload's relative path comes from each
part's `Content-Disposition: filename="..."`. The endpoint is parsed via
`axum_typed_multipart::TypedMultipart<DirectBuildForm>`, which streams
each part into a `tempfile::NamedTempFile` (RAII cleanup on early
return) instead of buffering the full payload in memory. Without
sanitisation, an authenticated org-member could submit
`filename="../../../../../etc/cron.d/owned"` and have the server write
attacker-controlled bytes anywhere the process can reach.
`validate_upload_filename` (in
`backend/web/src/endpoints/builds/direct.rs`) rejects any name whose
path components are not all `Component::Normal` — i.e. absolute paths,
parent (`..`) and current (`.`) components, Windows path prefixes, empty
strings, and embedded NUL bytes. The endpoint also re-checks that the
joined target stays under the per-upload temp directory as defence in
depth.

Unit tests (`backend/web/src/endpoints/builds/direct.rs::tests`):

- `accepts_simple_filenames` — `flake.nix`, `src/main.rs`, `a/b/c/d.txt`.
- `rejects_empty` — empty string is rejected.
- `rejects_parent_traversal` — `..`, `../etc/passwd`, deep traversal,
  and traversals embedded inside otherwise-normal paths
  (`foo/../../bar`, `foo/..`).
- `rejects_absolute_paths` — `/etc/passwd`, `/`.
- `rejects_current_dir_components` — `.`, `./foo`.
- `rejects_null_bytes` — NUL byte inside a filename.

Run with: `cargo test -p web --tests endpoints::builds::direct`

## Outgoing webhook URL — SSRF validation

`validate_webhook_url` (in `backend/core/src/ci/webhook.rs`) is the gate
between user-supplied webhook URLs and the outbound HTTP client. It is
called at create/update time (in `web::endpoints::webhooks::{put,
patch_webhook}`) and again at delivery time inside
`ReqwestWebhookClient::deliver`, which also performs DNS resolution and
rejects any resolved IP in a disallowed range. Redirects are disabled on
the production reqwest client.

Unit tests (`cargo test -p core --tests ci::webhook`):

- `validate_url_accepts_public_https` — `https://`/`http://` to public
  hostnames pass.
- `validate_url_rejects_invalid_scheme` — `file://`, `ftp://`,
  `gopher://`, `javascript:` are rejected.
- `validate_url_rejects_unparseable` — empty / non-URL strings rejected.
- `validate_url_rejects_localhost_name` — `localhost` (any case) is
  rejected.
- `validate_url_rejects_loopback_ipv4` — `127.0.0.0/8` blocked.
- `validate_url_rejects_aws_metadata_ip` — covers the motivating attack
  (`169.254.169.254`) plus the wider link-local block.
- `validate_url_rejects_rfc1918_ranges` — `10.x`, `172.16-31.x`,
  `192.168.x`.
- `validate_url_rejects_cgnat_shared_space` — `100.64.0.0/10` blocked,
  with boundary asserts that adjacent public space (`100.63.255.255`,
  `100.128.0.1`) is allowed.
- `validate_url_rejects_unspecified_and_broadcast` — `0.0.0.0`,
  `255.255.255.255`.
- `validate_url_rejects_multicast_ipv4` — `224.0.0.0/4`.
- `validate_url_rejects_reserved_ipv4` — `240.0.0.0/4`.
- `validate_url_rejects_ipv6_loopback_and_unspecified` — `::1`, `::`.
- `validate_url_rejects_ipv6_link_and_unique_local` — `fe80::/10`,
  `fc00::/7`.
- `validate_url_rejects_ipv6_multicast` — `ff00::/8`.
- `validate_url_rejects_ipv4_mapped_loopback_in_ipv6` — `::ffff:127.0.0.1`
  and `::ffff:169.254.169.254` blocked via the embedded-v4 check.
- `validate_url_accepts_public_ipv4_literal` /
  `validate_url_accepts_public_ipv6_literal` — sanity asserts that
  legitimate public IP literals (`8.8.8.8`, `2001:4860:4860::8888`) pass.

## CI reporter base URL — SSRF + redirect token leak (#113)

`GiteaReporter`, `GithubReporter`, and `GithubAppReporter` (in
`backend/core/src/ci/reporter.rs`) now validate any user-supplied
`base_url` / `api_base_url` through the same SSRF gate as outgoing
webhooks (`validate_webhook_url`), and build their reqwest clients with
`redirect::Policy::none()` so that an attacker cannot pivot a status
POST to an internal endpoint and leak the integration token via a
3xx `Location:` header. `reporter_for_project` continues to fall back
to `NoopCiReporter` when construction fails, with a `warn!` log.

Unit tests (`cargo test -p core --tests ci::reporter`):

- `gitea_reporter_rejects_aws_metadata_ip` /
  `github_reporter_rejects_aws_metadata_ip` /
  `github_app_reporter_rejects_aws_metadata_ip` — the motivating
  attack (`169.254.169.254`) is rejected by all three constructors.
- `gitea_reporter_rejects_localhost_hostname` /
  `github_reporter_rejects_localhost_hostname` — literal `localhost`
  rejected.
- `gitea_reporter_rejects_loopback_ipv4` /
  `github_reporter_rejects_ipv6_loopback` — `127.0.0.1`, `[::1]`
  rejected.
- `gitea_reporter_rejects_rfc1918` — `10.x`, `192.168.x` rejected.
- `gitea_reporter_rejects_non_http_scheme` — `file://`, `ftp://`
  rejected.
- `github_app_reporter_empty_url_still_uses_default` — empty string
  continues to fall back to `https://api.github.com` (the field is
  optional in `integration_lookup`).
- `reporter_for_project_unsafe_url_falls_back_to_noop` — an unsafe
  Gitea base URL plumbed through the factory degrades to
  `NoopCiReporter` rather than crashing the caller.

## GitLab outbound CI reporter (#90)

`GitlabReporter` (in `backend/core/src/ci/reporter.rs`) posts commit
statuses to GitLab via `POST {base_url}/api/v4/projects/{id}/statuses/{sha}`,
where `id` is the URL-encoded `owner/repo` path (also covers nested
groups such as `group/sub/repo`). Authenticates with `PRIVATE-TOKEN`,
which accepts personal, project, and group access tokens.

`resolve_outbound_reporter_for_project` (in
`backend/core/src/ci/integration_lookup.rs`) now constructs a
`GitlabReporter` for `ForgeType::GitLab` integrations instead of
returning a silent `NoopCiReporter`. Missing `endpoint_url` or access
token still falls back to `NoopCiReporter`, but with a `warn!` log so
operators can tell something is misconfigured.

Unit tests (`cargo test -p core --tests ci::reporter`):

- `gitlab_state_from_ci_status_all_variants` — every `CiStatus` maps
  to the documented GitLab state (`pending`, `running`, `success`,
  `failed`, with `Error` collapsed to `failed`).
- `gitlab_state_serializes_lowercase` — wire format matches the
  GitLab API enum.
- `gitlab_project_id_flat_path` /
  `gitlab_project_id_nested_groups` — `owner/repo` is URL-encoded as
  `acme%2Fwidgets`, and nested groups (`group/sub/repo`) become
  `group%2Fsub%2Frepo`.
- `gitlab_reporter_trims_trailing_slash` — base URL normalised.
- `gitlab_reporter_rejects_aws_metadata_ip` /
  `gitlab_reporter_rejects_localhost_hostname` /
  `gitlab_reporter_rejects_non_http_scheme` — same SSRF gate as the
  other reporters (`169.254.169.254`, `localhost`, `file://`).
- `reporter_for_project_gitlab_builds_gitlab` — the public factory
  builds a `GitlabReporter` for `ci_type="gitlab"`.

## SSH private key decryption — no plaintext fallback

`decrypt_ssh_private_key` in `backend/core/src/sources/ssh_key.rs`
decrypts the per-organization SSH key from `organization.private_key`.
Decryption failure must NOT silently fall back to interpreting the
stored value as a plaintext PEM, otherwise anyone with write access to
that column could bypass encryption entirely.

Tests (`backend/core/src/sources/ssh_key.rs`):

- `decrypt_ssh_key_corrupt_base64_fails` — non-base64 column rejected
  with `OrganizationKeyDecoding`.
- `decrypt_ssh_key_plaintext_pem_rejected` — a base64-encoded plaintext
  OpenSSH PEM placed directly in the column is rejected with
  `KeyDecryption`, not accepted.
- `decrypt_ssh_key_plaintext_non_pem_rejected` — random base64 garbage
  also fails with `KeyDecryption`.
- `generate_ssh_key_decrypts_to_openssh_pem` — properly encrypted keys
  still round-trip through decrypt.
## Body-size limits — webhook and direct-build (#51)

Without a body-size cap, `field.bytes().await` and the `body: Bytes`
extractor used by `forge_hooks` and `direct_build` would buffer entire
request bodies into memory, allowing a single 10 GB payload to OOM the
server. `create_router` (`backend/web/src/lib.rs`) now applies an
`axum::extract::DefaultBodyLimit::max(cli.max_request_size)` layer to the
whole API router (default 2 MiB) and overrides it on `POST /api/v1/builds`
with `cli.max_direct_build_size` (default 1 GiB) for legitimate
multi-file repository uploads.

Tests (`cargo test -p web --test body_size_limit`):

- `webhook_body_over_limit_returns_413` — a 4 KiB POST to
  `/api/v1/hooks/github` with `max_request_size = 1024` is rejected with
  `413 Payload Too Large` *before* the handler runs (so the OOM-prone
  `body: Bytes` read never happens).
- `webhook_body_within_limit_reaches_handler` — a 256 B body under the
  same 1 KiB cap is *not* short-circuited with 413; the handler runs and
  returns its normal response.
- `direct_build_route_uses_higher_limit` — a 16 KiB body to
  `POST /api/v1/builds` with `max_request_size = 1024` and
  `max_direct_build_size = 1 MiB` is *not* rejected with 413, proving
  the per-route override is wired up.
## Cache traffic metrics — atomic UPSERT (no lost updates)

`record_nar_traffic` (`backend/web/src/endpoints/stats.rs`) records bytes
served per `(cache, bucket_time)` row. The previous implementation used a
SELECT-then-UPDATE/INSERT pattern, which dropped updates whenever two NAR
fetches in the same minute bucket ran concurrently — both reads observed
the same `bytes_sent` value and the second writer clobbered the first
(see issue #50). It is now a single `INSERT … ON CONFLICT (cache,
bucket_time) DO UPDATE SET bytes_sent = bytes_sent + EXCLUDED.bytes_sent,
nar_count = nar_count + EXCLUDED.nar_count`, which Postgres serialises on
the unique index so every caller's increment is preserved.

Tests (`cargo test -p web --lib stats`):

- `record_nar_traffic_stmt_is_atomic_upsert` — asserts the generated SQL
  contains `INSERT INTO cache_metric`, `ON CONFLICT (cache, bucket_time)`,
  the additive `bytes_sent`/`nar_count` updates, and contains no `SELECT`
  (a `SELECT` would reintroduce the read-modify-write race).
## Worker-peer token verification — argon2 + constant time

Worker registration tokens are now stored as argon2 PHC strings rather
than bare hex SHA-256, and the handshake comparison runs in constant
time. `verify_token` in `backend/proto/src/handler/auth.rs` dispatches
on the stored format: PHC strings (starting with `$`) are verified via
`password_auth::verify_password`; legacy hex SHA-256 rows from
pre-existing registrations are accepted via a constant-time
`subtle::ConstantTimeEq` compare so old workers keep working until they
are re-registered. New tokens written by `POST /orgs/{org}/workers` and
by state-file provisioning use `password_auth::generate_hash`.

Backend tests (`cargo test -p proto --lib handler::auth`):

- `validate_tokens_argon2_hash_authorizes` — argon2-hashed registration
  authorises the matching plaintext token.
- `validate_tokens_argon2_wrong_token_fails` — argon2 row rejects
  wrong tokens with `"invalid token"`.
- `verify_token_dispatches_on_format` — `$argon2…` routes to
  `password_auth`; lowercase hex routes to constant-time SHA-256.
- The pre-existing `validate_tokens_*` tests using `sha256_hex` continue
  to cover the legacy-format compatibility path.

## Sign sweep — batched, bounded, single crypt-secret read (#105)

`sign_missing_signatures` (in `backend/cache/src/cacher/sign_sweep.rs`)
used to issue 2 SELECTs per pending row (cache + cached_path), reload
the crypt-secret file from disk, and re-decrypt each cache's private
key on every row, with no `LIMIT` on the initial query — at scale this
became 50k+ DB calls plus 50k+ crypt-secret reads per minute, and a
single backlog could pin one DB connection indefinitely.

The sweep is now `LIMIT`-bounded (`SIGN_SWEEP_BATCH = 1000` rows per
pass) and batches the `cache` / `cached_path` lookups into one
`is_in(...)` query each. Per-cache decrypted keys are wrapped in a new
`CacheSigner` (in `backend/core/src/sources/cache_key.rs`) built once
per pass per cache — the crypt secret is read at most once per cache,
not once per signature. `sign_narinfo_fingerprint` is now a thin
one-shot wrapper around `CacheSigner::sign_narinfo` so existing
callers keep working byte-for-byte.

Unit tests (`cargo test -p core --lib sources::cache_key`):

- `cache_signer_matches_one_shot_signer` — for several
  `(store_path, nar_hash, nar_size, refs)` tuples, asserts that the
  signature produced by `CacheSigner::sign_narinfo` is byte-identical
  to the one produced by `sign_narinfo_fingerprint`. Guards against the
  batching refactor silently changing the on-wire fingerprint.
- `cache_signer_rejects_bad_key_at_build_time` — a cache row whose
  `private_key` cannot be base64-decoded fails at
  `CacheSigner::from_cache`, so the sweep can mark the cache as
  unsignable for the rest of the pass instead of repeating the
  decryption error per row.

The pre-existing `cacher::sign_sweep::tests::hex_hash_to_nix32_*`
suite continues to cover the hash-format conversion path.

## Proto WebSocket — message-size cap & handshake timeout

The `/proto` WebSocket caps every inbound and outbound frame at
`MAX_PROTO_MESSAGE_SIZE` (1 MiB) — applied to both the inbound
`axum::extract::ws::WebSocketUpgrade` and the outbound
`tokio_tungstenite::connect_async_with_config` call in
`backend/proto/src/outbound.rs`. The cap comfortably exceeds any legitimate
frame (`NarPush` chunks are 64 KiB plus rkyv overhead) while preventing a
peer from forcing a multi-megabyte allocation per message.

`handle_socket` additionally wraps the entire handshake
(Discoverable check → InitConnection → AuthChallenge → AuthResponse →
InitAck) in a `HANDSHAKE_TIMEOUT` (15 s) `tokio::time::timeout`, so a peer
that completes the WebSocket upgrade and then stalls cannot pin a tokio task
or file descriptor indefinitely.

Tests (`cargo test -p proto`):

- `tests::max_proto_message_size_is_sane` — regression for #110: cap stays
  at least `2 × NAR_PUSH_CHUNK_SIZE` (room for chunk + framing) and well
  below 16 MiB so a future refactor cannot silently relax the bound back
  toward tungstenite's 64 MiB default.
- `tests::handshake_timeout_is_sane` — regression for #110: deadline stays
  in `[5 s, 60 s]` so a real auth round-trip still fits but a stalled peer
  is dropped quickly.

## Worker — reconnect retries forever

`Worker<Disconnected>::reconnect` (`backend/worker/src/worker/mod.rs`) now
returns `Result<Worker<Connected>, (anyhow::Error, Self)>`: on failure, the
disconnected typestate (and the cached executor / scorer / credentials /
candidate maps) is handed back so the caller can retry without losing
state. The reconnect-with-backoff loop in `main.rs` is extracted to
`backend/worker/src/reconnect.rs::retry_reconnect` so it is unit-testable
without standing up a real `Worker`. The loop never gives up — a transient
network blip cannot terminate the worker process anymore (#99).

Tests (`cargo test -p worker --bins reconnect`):

- `reconnect::tests::keeps_retrying_after_failure` — regression for #99:
  the loop returns `Ok` only after several failed attempts, so a single
  transient error no longer breaks out and shuts the worker down.
- `reconnect::tests::backoff_caps_at_max` — delay sequence doubles from the
  initial backoff and plateaus at `max_backoff`.
- `reconnect::tests::state_threads_through_retries` — the same state value
  is threaded through every attempt, proving the typestate-preservation
  contract that the real `Worker<Disconnected>` relies on for cached
  resources.

## Typed DB pools — `WebDb` / `WorkerDb`

`ServerState` previously held two raw `DatabaseConnection` fields named `db`
and `web_db`; nothing in the type system stopped a web handler from
reaching for `state.db` (the proto/scheduler/cache pool) or vice versa.
The `db` field is now `worker_db: WorkerDb` and `web_db: WebDb`
(`backend/core/src/types/db.rs`). Both newtypes forward `ConnectionTrait`
to the inner pool so existing call sites
(`find().one(&state.web_db)`, `state.worker_db.execute(stmt)`, …) work
unchanged. The compile-time defense kicks in at any function boundary
that types its parameter explicitly as `&WebDb` or `&WorkerDb`: the two
newtypes are non-substitutable.

While auditing, one inconsistency was fixed in
`web::endpoints::stats::get_cache_stats` — the cache-totals query was
reading from the worker pool while every other query in the same handler
used `web_db`; it now uses `web_db` consistently. The fire-and-forget
NAR-fetch bookkeeping in `web::endpoints::caches::nar` keeps using
`worker_db` on purpose (it should not contend with foreground HTTP
requests) and now carries a comment explaining the choice.

Tests (`cargo test -p core --lib types::db`):

- `types::db::tests::newtypes_are_non_substitutable` — regression for
  #68: a function typed `fn(&WebDb)` must not accept a `&WorkerDb` and
  vice versa, which is the compile-time defense the issue asked for.
- `types::db::tests::forwards_connection_trait` — `&WebDb` / `&WorkerDb`
  satisfy `&impl ConnectionTrait`, so existing SeaORM call sites keep
  working without `.inner()` boilerplate.

## Build status — `Created` collapsed to `Queued` for API responses

Issue #120: the frontend renders a coloured dot via
`status-{{ build.status.toLowerCase() }}`, but no `status-created` style
exists. `Created` is an internal-only transient state — the scheduler
flips builds to `Queued` almost immediately — so the API now collapses
it via `BuildStatus::for_api()` (`backend/entity/src/build.rs`) at every
response boundary (`evals::query`, `projects::evaluations`,
`projects::metrics`, `builds::query`).

Unit tests in `backend/entity/src/build.rs`:

- `for_api_collapses_created_to_queued` — `Created.for_api() == Queued`.
- `for_api_passes_through_other_states` — every other variant is
  returned unchanged.

## Shared web/core helpers (`#78`)

To collapse the boilerplate measured in issue #78, the following helpers
were introduced and applied repo-wide:

- `core::types::now()` — single source for `chrono::Utc::now().naive_utc()`,
  the timestamp shape every persisted column expects.
- `web::helpers::ok_json(message)` — wraps a value in the standard
  successful `BaseResponse` envelope, replacing the boilerplate
  `Json(BaseResponse { error: false, message })`.
- `web::helpers::OptionExt::or_not_found(resource)` — converts the
  result of a SeaORM `.one(db).await?` lookup into a `WebResult<T>`
  with a `<resource> not found` 404, replacing the
  `.ok_or_else(|| WebError::not_found(...))` chain.
- `WebError::{bad_request, unauthorized, forbidden, conflict,
  unprocessable_entity, internal, service_unavailable}` — accept
  `impl Into<String>` so callers can drop `.to_string()` on string
  literals and `format!(...)` payloads.
- `WebError::data_inconsistency(resource)` — for the recurring
  `"<resource> data inconsistency"` referential-integrity 500.

Unit tests in `backend/web/src/helpers.rs`:

- `ok_json_wraps_with_error_false` — the envelope is constructed with
  `error: false` and the supplied message.
- `or_not_found_returns_value_for_some` — passes the inner value through
  unchanged.
- `or_not_found_maps_none_to_not_found` — produces the expected
  `WebError::NotFound("Thing not found")`.

## Shared HTTP client (`#79`)

Eliminates the prior 18 ad-hoc `reqwest::Client::new()` /
`reqwest::Client::builder()` constructions across the workspace, which
each created a fresh TCP/TLS connection pool with inconsistent (or
absent) timeout and redirect policy.

`backend/core/src/http.rs` builds the project-wide client with sane
defaults (30 s timeout, `redirect::none`, `gradient/<version>`
user-agent). The server stores it once on `ServerState::http`; the
worker exposes it through a `OnceLock` (`worker::http::client()`); the
CLI exposes it through `connector::http_client()`.

CI reporters (`GiteaReporter`, `GithubReporter`, `GithubAppReporter`)
and the GitHub-App helpers (`get_installation_token`, `exchange_code`)
now take the shared `reqwest::Client` as a parameter instead of building
their own.

Unit tests in `backend/core/src/http.rs`:

- `build_client_succeeds` — the default builder yields a usable
  `reqwest::Client`.
- `user_agent_is_prefixed` — the user-agent string is namespaced
  `gradient/...` so server logs can identify outbound calls from
  Gradient processes.

## Graceful shutdown (`#72`)

`backend/core/src/shutdown.rs` introduces a `Shutdown` primitive bundling a
`tokio_util::sync::CancellationToken` with a `tokio_util::task::TaskTracker`.
It replaces bare `tokio::spawn` for every long-lived background task —
dispatch loops, the outbound worker connection loop, the cache GC and
sign-sweep loops, webhook deliveries, CI reporters, and the fire-and-forget
metric writes from the NAR cache surface. `serve_web` installs a
SIGINT/SIGTERM handler that calls `shutdown.cancel()`, hands the token to
`axum::serve(...).with_graceful_shutdown(...)`, then awaits
`shutdown.cancel_and_drain(30s)` so in-flight cleanups, metric writes, and
webhook deliveries finish before the process exits.

Unit tests in `backend/core/src/shutdown.rs`:

- `cancel_interrupts_select_loop` — a task that `select!`s on
  `cancelled()` against a 60-second sleep returns immediately when the
  token fires.
- `drain_waits_for_in_flight_work` — `cancel_and_drain` waits for
  spawned futures to finish (no abandonment of in-flight work).
- `drain_timeout_returns_false` — a task that ignores the cancel
  signal is reported as a drain timeout, not silently abandoned.
- `child_token_cascades_from_parent` — child tokens used for
  per-connection / per-job scopes cancel transitively.

## Shared transitive-dependents walk (`#108`)

`backend/core/src/db/dependency_graph.rs` exposes
`collect_transitive_dependents`, the single canonical reverse-edge BFS over
the `derivation_dependency` table. Both the cache-invalidation closure
revocation in `cache::cacher::invalidate::revoke_cache_derivation_closure`
and the build-failure cascade in
`scheduler::build::BuildStateHandler::cascade_dependency_failed` now route
through it instead of carrying their own copy. The cascade also collapses
to a single batched `derivation IS IN (...)` builds query, replacing the
prior per-iteration full re-scan + per-build edge probe.

Unit tests in `backend/core/src/db/dependency_graph.rs`:

- `no_dependents_returns_only_start` — a leaf derivation yields a set
  containing exactly the starting id.
- `walks_multiple_layers_breadth_first` — a 3-layer graph is fully
  visited, including a sibling that depends directly on the start.
- `cycles_terminate` — a pathological reverse cycle is deduped via the
  visited set so the BFS terminates.

## Build deduplication via `via` field (`#175`)

When two evaluations (in the same organisation) discover the same
derivation, the second build is inserted as a *follower* of the first by
storing the leader's id in the new `build.via` column. Followers are
filtered out of `dispatch_ready_builds` SQL (`AND b.via IS NULL`), so two
workers never race for the Nix store lock on the same output path. When a
leader transitions to `Completed`, `Substituted`, `Failed`, or
`DependencyFailed`, `propagate_to_followers` copies the terminal status,
`log_id`, `build_time_ms`, and `worker` onto every follower, runs the
per-evaluation cascade for failure cases, and finalises each follower's
evaluation. `Aborted` is never propagated — when a leader is aborted (its
own evaluation cancelled) `abort_evaluation` re-elects a new leader from
the surviving followers instead of dragging unrelated evaluations down.

Tests:

- `dispatch_tests::dispatch_skips_follower_builds` — the SQL gate keeps
  followers out of the dispatcher result set, so no follower job is ever
  enqueued.
- The full pre-existing `handle_build_job_completed` /
  `handle_build_job_failed` mock-DB suite was extended to mock the
  `propagate_to_followers` followers query, exercising the new code path
  on every terminal transition.

## Typed entity IDs (`entity::ids`)

`backend/entity/src/ids.rs` defines one newtype per entity (`UserId`,
`OrganizationId`, `ProjectId`, …) so the compiler rejects argument
swaps. Unit tests (`cargo test -p entity --tests`) cover:

- Round-trip with `Uuid` (no information loss).
- `serde` transparency (wire format identical to bare `Uuid`).
- `FromStr` parsing (lets axum `Path<UserId>` extract from URL segments).
- `TryFromU64` returns `DbErr` (UUID PKs are never `u64`-derivable).

A `trybuild` compile-fail test
(`cargo test -p entity --test compile_fail`) locks the swap-prevention
property: a function expecting `OrganizationId` MUST reject a `UserId`
argument at compile time. Regenerate the captured rustc diagnostic
after a deliberate API change with:

    TRYBUILD=overwrite cargo test -p entity --test compile_fail

## NAR streaming — bounded backend reads

`core/src/storage/nar.rs::tests`:

- `get_stream_returns_full_payload_in_order` writes a 9 MiB payload through
  `NarStore::put`, re-reads it via the new `get_stream` API, and verifies
  the concatenated chunks match the original bytes. This is the contract
  relied on by the WebSocket NAR-serving path: the server must *never*
  load the full file into RAM, but the bytes on the wire still have to
  round-trip exactly.
- `get_stream_returns_none_for_missing` confirms that absent objects
  surface as `Ok(None)` so the caller can emit `NarUnavailable` instead
  of hanging.

## Proto writer — peer-stall detection

`proto/src/handler/socket.rs::writer_tests`:

- `send_msg_times_out_when_queue_is_full` constructs a `ProtoWriter`
  whose drain task is intentionally absent, fills the bounded queue, and
  asserts the next `send_msg` returns `Err(())` after
  `send_chunk_timeout` instead of blocking forever. This is the
  producer-observable signal that a peer's TCP receive side has stalled
  — the failure unblocks the dispatch loop instead of letting the
  worker's 600 s receive ceiling fire.
- `send_msg_succeeds_when_queue_has_room` covers the fast path: a
  serialised message lands in the channel without delay when there's
  capacity.

## Proto NAR serving — streaming, chunking, and missing paths

`proto/src/handler/socket.rs::serve_nar_tests`:

- `serve_streams_full_payload_in_chunks` puts a 9 MiB NAR into a local
  `nar_storage`, calls `serve_nar_request`, and asserts the spy writer
  observed ≥ 3 `NarPush` frames whose concatenated `data` equals the
  source. The last frame must have `is_final = true`. Locks the
  invariant that streaming serving preserves wire semantics.
- `serve_emits_nar_unavailable_when_missing` confirms a missing hash
  surfaces as exactly one `NarUnavailable` frame plus an `Err` return —
  no `NarAbort`, no orphan `NarPush`.

## Per-session NAR upload buffer — bounded memory (issue #109)

`proto/src/handler/dispatch.rs::nar_buffers_tests`:

- `append_below_budget_succeeds_and_tracks_total` exercises the happy
  path and the running byte counter.
- `append_overflow_returns_err_and_does_not_mutate` makes the rejection
  guarantee explicit: a push that would breach the cap returns `Err` and
  leaves the buffer state unchanged. The dispatcher uses this to abort
  the offending job with `AbortJob` instead of accepting the bytes.
- `take_releases_budget` asserts that finalising a path frees the byte
  budget so subsequent uploads on the same session aren't blocked.
- `take_missing_returns_none` covers the presigned-S3 path where no
  `NarPush` chunks were ever buffered.
- `append_overflow_across_keys_is_caught` proves the cap is a *session*
  budget, not a per-path one — many small open uploads cannot collude
  to exceed the limit.

## Auth hardening — sessions, API key lifecycle, account deletion (issue #91)

`backend/web/tests/auth_hardening.rs` drives the production router with a
`MockDatabase` and signs synthetic JWTs against the same secret the test
state holds. Each test pins one revocation/expiry rule to a specific HTTP
status so a regression cannot quietly weaken the surface:

- `jwt_with_revoked_session_is_rejected` and `jwt_with_expired_session_is_rejected`
  prove that a JWT alone is no longer sufficient — the auth middleware
  loads the matching `session` row and refuses anything revoked or past
  `expires_at`. This is what makes logout effective (issue #104).
- `jwt_with_unknown_session_is_rejected` covers the case where the row was
  deleted: the token must fail closed.
- `revoked_api_key_is_rejected` and `expired_api_key_is_rejected` lock in
  the same checks for `GRAD…` keys (issue #44). A revoked or expired key
  returns 401 even if the hash still matches.
- `delete_user_without_password_is_forbidden` and
  `delete_user_with_wrong_password_is_forbidden` enforce the re-auth
  requirement on `DELETE /user` — a stolen JWT cannot wipe a
  password-auth account on its own (issue #43).

Run with `cargo test -p web --test auth_hardening`.

## Evaluation `waiting_reason` — surfaces the reconciler verdict (issue #98)

`backend/scheduler/src/build.rs::waiting_reason_tests` exercises
`BuildabilityChecker::compute_waiting_reason` directly so the API payload
returned by `GET /evals/{evaluation}` is locked in:

- `no_workers_lists_every_unique_arch` — when no worker is connected, every
  pending build's `(architecture, required_features)` combo lands in
  `unmet`, with `connected_workers == 0`.
- `satisfied_builds_are_excluded_from_unmet` — pending builds whose arch
  matches some connected worker are filtered out; only the genuinely
  blocked combos remain.
- `missing_feature_is_reported_alongside_arch` — a build whose arch is
  available but whose `requiredSystemFeatures` aren't satisfied is
  reported with the missing feature names attached.
- `identical_requirements_are_grouped_with_count` — N pending builds with
  the same blocking requirement collapse to one `UnmetRequirement` with
  `build_count == N`, so the UI doesn't repeat itself.
- `builtin_arch_satisfied_by_any_worker` — `architecture == "builtin"`
  derivations are never counted as unmet so long as any worker is
  connected.

Run with `cargo test -p scheduler --tests waiting_reason_tests`.

## Project triggers (issue #116)

- `core::types::triggers` — round-trip serialisation, polling interval validation (≥10s), polling branch field (optional, nullable), six-field cron parsing, type/JSON shape mismatches.
- `core::ci::abort` — `abort_evaluation` hard vs soft, terminal eval no-op.
- `core::ci::apply` — `apply_trigger` orchestration: same-commit dedup, time-trigger and manual bypass, project-level concurrency policies (skip / hard_abort / soft_abort / all). The `all` policy creates a new evaluation alongside a running one; the new row carries `concurrent = true`.
- `core::state::provisioning` — trigger config builder helpers, integration name resolution, key stability.
- `scheduler::trigger_dispatch` — `polling_due` and `cron_due` boundary conditions; `dispatch_once` no-trigger and within-interval skip cases.
- `scheduler::jobs::JobTracker::remove_job` — pending and active map removal; unknown id no-op.
- `scheduler::Scheduler::cancel_evaluation_jobs` — drops eval and per-build entries from the tracker.
- `web::endpoints::projects::triggers` — list/create/read/update/delete; `all` concurrency accepted (200); invalid config rejected (400).
- `web::endpoints::projects::evaluations` — response includes nullable `trigger` summary, populated for evaluations created by a trigger.
- `web::endpoints::forge_hooks::events` — PR (github/gitea/gitlab) and release (github/gitea/gitlab) parsers; GitLab action mapping; tag-ref support on push parsers.
- `web::endpoints::forge_hooks` integration — push fans out to matching trigger row; branch glob filter skip; PR action filter; release fires only `releases_only` triggers; GitHub App push by installation_id.
- `web::endpoints::projects::management` — creating a project seeds a default polling trigger.

## Proto wire decoders — alignment-safe deserialisation

`rkyv::from_bytes` requires the input slice to be aligned to the archive's
required alignment (16 bytes for `ClientMessage` / `ServerMessage`), but
inbound WebSocket buffers (`axum::body::Bytes`, `tungstenite::Message::Binary`)
only guarantee `align_of::<u8>() == 1`. Decoding a bare `&[u8]` therefore
fails non-deterministically depending on the allocator's output, surfacing
on the server as `proto::handler::socket: failed to deserialize client
message` and on the worker as `Connection reset without closing handshake`
once the server tears down the socket.

`backend/proto/src/messages/wire.rs` provides `decode_client_message` and
`decode_server_message`, both of which copy inbound bytes into an
`AlignedVec<16>` before calling `rkyv::from_bytes`. Every WebSocket-receive
path in the workspace funnels through these helpers; open-coding
`rkyv::from_bytes` on raw network bytes is the bug they exist to prevent.

Tests (`cargo test -p proto`):

- `messages::wire::tests::decode_client_message_handles_misaligned_input`
  and `…::decode_server_message_handles_misaligned_input` —
  encode a representative message, place the bytes at a deliberately
  misaligned address (`AlignedVec<16>` base + 1) so the input pointer is
  guaranteed not to be 16-byte-aligned, then assert the helper still
  decodes back to the original value. This is the regression for the
  reconnect-time deserialisation failures observed when the server's
  inbound buffer happened to land at a non-16-byte-aligned allocator
  address.

## Cache GC — guard shared-hash NARs and purge zombie cached_path rows

Two bugs together inflated cache stats and over-deleted shared NARs:

- `gc_orphan_derivations` deleted the NAR for every output of every orphan
  derivation, with no check whether another (non-orphan) `derivation_output`
  shared the same hash via `cached_path`. FOD source tarballs are the
  textbook case — `fetchurl` derivations across many projects all
  reference the same `<hash>-<name>`, so when one drv aged into the
  orphan window its NAR vanished for everyone. Fixed by collecting all
  orphan output hashes, subtracting hashes still referenced by any
  non-orphan `derivation_output`, and only deleting the difference (NAR
  file plus `cached_path` row; `cached_path_signature` cascades).
- The previous code didn't drop `cached_path` rows at all. Each
  over-deletion left a row behind that `cached_path_signature` still
  pointed at, so `COUNT(cps.id)` in `web/src/endpoints/stats.rs` reported
  packages whose NARs were long gone. `cleanup_orphaned_cache_files` now
  finishes with a `purge_zombie_cached_paths` pass: any `cached_path`
  with `file_hash IS NOT NULL` whose hash is not in the on-disk list is
  deleted, dragging its signature placeholders along via cascade.

Tests (`cargo test -p cache --tests cacher::cleanup`):

- `purges_cached_paths_whose_nar_is_missing` — feeds a `cached_path`
  whose hash is absent from the local NAR store and asserts the live
  NAR is preserved while the orphan-files pass exercises the new
  cleanup branch.

## Substituted classification — match cached_path by hash, not by foreign-key link

`compute_truly_substituted` previously demanded
`derivation_output.cached_path IS NOT NULL` and `is_cached = true` to mark
a drv as `Substituted`. That link is set lazily by `mark_nar_stored` on
upload, so a re-evaluated drv whose output hash was already in
`cached_path` (shared FOD source, manual cache push, fresh eval before
its first upload) was misclassified as needing a build and rerun every
time. The worker's `CacheQuery` handler already merges by hash for the
same reason; the eval-time decision now does too.

Tests (`cargo test -p scheduler --tests substitut`):

- `eval_result_substituted_derivation_completes_eval` — original happy
  path: linked cached_path with file_hash → drv marked Substituted, eval
  completes immediately.
- `eval_result_substitutes_when_hash_in_cached_path_without_link` —
  regression: derivation_output with `is_cached = false` and
  `cached_path = None`, but a `cached_path` row with the same hash and
  `file_hash IS NOT NULL` exists. The drv is marked Substituted and the
  eval completes without dispatching a build. Confirms the hash-based
  fallback in `compute_truly_substituted`.

## Build artefacts — `external_cached` outputs include `hydra-build-products`

Builds that are dispatched as `external_cached` (substituted from upstream,
not rebuilt locally) used to report `products: Vec::new()` even when the
fetched output contained `nix-support/hydra-build-products`, leaving the
artefacts page empty for any drv that was already on `cache.nixos.org`.
The worker's external-cache branch now calls `load_products` on each
fetched output path, the same loader the regular build path uses.

Tests (`cargo test -p worker executor::build::tests`):

- `load_products_returns_empty_when_file_absent` — the loader is a no-op
  when the output has no `nix-support/hydra-build-products`, so substituted
  outputs without artefacts remain artefact-free.
- `load_products_parses_hydra_lines` — a `file html …/index.html` line in
  `nix-support/hydra-build-products` produces one `BuildProduct` with the
  `file_type`, `subtype`, `name` (basename), and `size` (stat) populated.
  Regression for substituted/external-cached builds whose artefacts never
  reached the `build_product` table.

## CI pending status fires at queue time (#117)

The top-level `gradient` CI status used to first appear on a commit only
when a worker picked the evaluation up and it transitioned `Queued →
Fetching`. During the gap between insert and worker pickup the commit
showed no status, hiding that work had been scheduled. The scheduler now
spawns a `Pending` report from `scheduler::ci::spawn_pending_ci_for_eval`
at every site that creates a `Queued` evaluation via `apply_trigger`
(scheduler trigger dispatch, manual API fire, forge webhook fan-out). The
existing `Running`-on-`Fetching` transition is preserved and updates the
same check run id.

Tests (`cargo test -p scheduler --tests ci::tests`):

- `pending_ci_skips_when_eval_has_no_project` — direct builds and other
  project-less evaluations don't get a CI report; the helper returns
  without spawning so the shutdown tracker stays empty.
- `pending_ci_spawns_task_when_eval_has_project` — when the evaluation
  has a project, the helper registers a task on the shutdown tracker so
  `cancel_and_drain` covers the in-flight report on shutdown.

## Enum primitive conversions via `num_enum` (#80)

`BuildStatus`, `EvaluationStatus`, `IntegrationKind`, `ForgeType`,
`TriggerType`, and `ConcurrencyPolicy` derive
`num_enum::IntoPrimitive`/`TryFromPrimitive` instead of hand-rolled
`as_i16`/`from_i16`/`num_value` helpers. Database rows still use the
explicit discriminants — moving them in source would silently break the
on-disk encoding.

The `concurrency_round_trip` and `trigger_type_round_trip` tests in
`core/src/types/triggers.rs` cover the integer ↔ enum mapping and assert
that out-of-range values produce an error rather than panicking.

## `GET /commits/{commit}` authorization (#88)

The endpoint historically returned commit metadata to any authenticated
caller — the handler held a `// TODO: Check if user has access to the
commit` and never enforced it, allowing cross-tenant disclosure of
commit message, hash, and author for any commit UUID an attacker could
guess or harvest. The route now lives behind `authorize_optional` and
the handler walks `commit → evaluation → project|direct_build →
organization` to require either public visibility or membership; every
other case (non-member, anonymous on private org, missing commit, no
referencing evaluation) maps to `404` so existence isn't leaked.

Tests (`cargo test -p web --test commits_authorization`):

- `anon_can_read_commit_in_public_org` — an unauthenticated caller may
  fetch a commit reachable through a project in a public organization.
- `anon_cannot_read_commit_in_private_org` — the same commit, but the
  organization is private, returns `404` for an unauthenticated caller.
- `member_can_read_commit_in_private_org` — an authenticated member of
  the owning organization sees the commit (200).
- `non_member_cannot_read_commit` — an authenticated user who is not a
  member of any organization that owns a referencing evaluation gets
  `404`. Direct regression for #88.
- `member_can_read_commit_referenced_via_direct_build` — when the only
  reachable evaluation has no `project` (direct build), the handler
  resolves the org via the `direct_build` row and grants access.
- `nonexistent_commit_returns_404` and
  `commit_without_evaluation_returns_404` — both shapes of "no path"
  return `404` without leaking which case applied.

## Proto WebSocket connection cap (#89)

`max_proto_connections` (env `GRADIENT_MAX_PROTO_CONNECTIONS`, default
256) was previously declared as configuration but never read — workers
could open `/proto` WebSockets without bound, exhausting file
descriptors, scheduler slots, and memory. The proto upgrade handler now
holds a permit on a `ProtoLimiter` (a `tokio::sync::Semaphore` sized
from the config) for the lifetime of each connection; once the limit is
hit, further upgrade attempts get `503 Service Unavailable` with
`Retry-After: 10`.

Unit tests (`cargo test -p proto handler::limiter`):

- `new_clamps_zero_capacity_to_one` — a misconfigured `0` collapses to
  `1` so the endpoint never silently rejects every upgrade; operators
  who want the endpoint disabled set `discoverable = false`.
- `try_acquire_returns_none_when_exhausted` — at capacity the next
  acquire fails immediately rather than queueing.
- `dropping_permit_releases_slot` — the slot is reclaimed when the
  permit is dropped, which corresponds to the upgraded session ending.
- `in_use_tracks_held_permits` — the operator-visible `in_use()` count
  matches the number of live permits (used in the rejection log line).

Integration tests (`cargo test -p web --test proto_connection_limit`)
cover the wiring of the limiter into the proto router:

- `upgrade_rejected_with_503_and_retry_after_when_limit_exhausted` — a
  WS-shaped GET against a saturated limiter returns `503` with the
  documented `Retry-After: 10` header. Direct regression for #89.
- `upgrade_proceeds_past_limiter_when_slot_is_free` — a fresh limiter
  does not produce the rejection response, confirming the handler only
  short-circuits on exhaustion.
- `slot_is_released_for_subsequent_upgrades_after_drop` — a held permit
  forces the first upgrade to `503`; dropping it lets the next request
  through, confirming the permit lifetime is what gates the slot.

## DB transactions for multi-step writes (issue #64)

Several create/update handlers historically performed two or more
inserts back-to-back without a transaction, so a unique-constraint
collision or other failure on the second statement would leave the
first row committed (orphaned org without admin membership, cache
without its default upstream, direct-build row without its uploaded
artefacts on disk, etc.). The handlers now wrap each multi-step write
in a `sea_orm` transaction and map PostgreSQL `unique_violation`
(SQLSTATE `23505`) onto a typed `WebError::already_exists` via a shared
helper.

Integration tests in `backend/web/tests/orgs_create.rs` and
`backend/web/tests/caches_create.rs` cover the two `put` handlers that
gained transactional scope:

- `orgs::put` now wraps the `organization` insert and the admin
  `organization_user` membership insert in a single transaction.
- `caches::put` now wraps the `cache` insert and the default
  `cache_upstream` insert in a single transaction.

Each file contains a regression test that exercises the in-handler
pre-check and asserts the `409 already_exists` envelope on a duplicate
name, plus a happy-path test that drives the new `tx + commit` code
path end-to-end and asserts both rows are present afterwards.
`MockDatabase` does not simulate transactional rollback, so these tests
verify call sequence and error mapping; the rollback contract itself
is a SeaORM trust boundary.

Unit tests for the SQL-error mapping live in `backend/web/src/error.rs`:

- `from_db_err_passes_through_non_db_errors` — non-`DbErr::Query`
  variants (e.g. `RecordNotFound`) round-trip as `WebError::Internal`
  rather than being misclassified as conflicts.
- `from_db_err_passes_through_query_string_errors` — a `DbErr::Query`
  carrying a string-only payload (no underlying `sqlx::Error`) is
  treated as `Internal`.
- `from_db_err_record_not_found_is_internal` — pins the documented
  behaviour that "row missing" is the caller's pre-check problem, not
  a 409.

The mapper uses the typed sqlx 0.8 API (`db_err.is_unique_violation()`)
rather than scraping `to_string()`, so it survives sqlx upgrades that
reflow the message text.

Unit tests for the `TempUploadDir` RAII guard used by the direct-build
upload path live in `backend/web/src/endpoints/builds/direct.rs`:

- `temp_upload_dir_drop_removes_directory` — dropping the guard
  without calling `commit()` removes the on-disk staging directory, so
  a failed DB transaction cannot leave orphaned NARs behind.
- `temp_upload_dir_commit_keeps_directory` — `commit()` consumes the
  guard and leaves the directory in place, matching the contract that
  the upload only becomes "real" once the surrounding transaction has
  committed.

## Request-id correlation across handler / DB / cleanup tasks (#86)

Without a stable per-request id, a single HTTP request that issues four
DB queries plus a webhook delivery emits five unrelated log lines, and
cleanup work spawned through `Shutdown::spawn` loses the request
context entirely. `create_router` (`backend/web/src/lib.rs`) now wires
`SetRequestIdLayer` (with a `MakeRequestUuid` UUID-v7 minter),
`TraceLayer::make_span_with` (which opens an `http_request` span
carrying `method`, the `MatchedPath` route, and the `x-request-id`),
and `PropagateRequestIdLayer` (which echoes the id on the response).
`Shutdown::spawn` (`backend/core/src/shutdown.rs`) wraps every spawned
future with `.in_current_span()`, so cleanup tasks inherit the request
span and the id is on every line they emit.

Tests (`cargo test -p web --test request_id`):

- `missing_request_id_is_generated` — a request without `x-request-id`
  comes back with one, and the value parses as a UUID. Confirms the
  `MakeRequestUuid` minter is wired in front of `TraceLayer`.
- `supplied_request_id_is_echoed` — a request that *does* carry
  `x-request-id` has its value preserved verbatim on the response, so a
  reverse-proxy that injects an upstream trace id keeps the trace
  stitched end-to-end.
- `each_request_gets_a_distinct_id` — successive auto-generated ids
  differ; otherwise log correlation collapses across concurrent
  requests on the same connection.

## FK-chasing data-inconsistency log level (#85)

Access-context loaders (`EvalAccessContext` in
`backend/web/src/endpoints/evals/mod.rs`, `BuildAccessContext` in
`backend/web/src/endpoints/builds/mod.rs`, the derivation lookup in
`backend/web/src/endpoints/builds/query.rs`) chase from a child row to
its parent through FK columns. When the parent is missing — almost
always a transient race against a concurrent delete — the previous
implementation logged the event twice at error level: once at the
callsite and again inside `WebError::IntoResponse` for the wrapping
`Internal` variant. That noise drowned legitimate server errors.

The fix introduces a dedicated `WebError::DataInconsistency` variant.
External behaviour is unchanged (HTTP 500, code `internal`, body
`Internal server error`); the difference is operational:

- `IntoResponse` no longer logs for the new variant — the rich-context
  warn line is emitted exactly once at the construction callsite,
  carrying the structured ids (`project_id`, `evaluation_id`,
  `derivation_id`, `organization_id`) that triage actually needs.
- The callsite log itself is now `tracing::warn!`, not `error!`, so
  these benign races no longer pollute the error stream.

No new tests were added: the change is a log-level adjustment and a
no-op refactor of the error variant. The existing access-control tests
(`evaluations.rs`, `builds_download.rs`, `commits_authorization.rs`)
still cover the response side of the data-inconsistency paths and pass
unchanged.
