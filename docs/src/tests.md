# Tests

This page tracks notable tests added to Gradient and where they live.

## OIDC - CSRF cookie, ID-token verification, identity binding

Tests in `backend/web/src/authorization/oidc.rs` cover the security
fixes for issue #38:

- `random_url_safe_is_unique_and_url_safe` - `state`/`nonce` are
  cryptographically random and URL-safe.
- `csrf_cookie_roundtrips` / `csrf_cookie_rejects_wrong_secret` /
  `csrf_cookie_rejects_expired` - the `oidc_csrf` cookie is an
  HMAC-signed JWT that round-trips, fails verification under a
  different secret, and is rejected when expired.
- `state_compare_constant_time_rejects_mismatch` - `state` comparison
  uses `subtle::ConstantTimeEq`.

The full ID-token verification path (signature against the provider's
JWKS, `iss`/`aud`/`exp`/`nonce` checks, identity bound to
`(oidc_issuer, oidc_subject)` rather than email) is enforced in
`oidc_login_verify` and exercised end-to-end via the
`/auth/oauth/authorize` and `/auth/oidc/callback` endpoints.

`backend/web/tests/oidc_pkce.rs::authorize_redirect_carries_pkce_and_cookie_holds_verifier`
asserts the login redirect carries `code_challenge` + `code_challenge_method=S256`
and that the verifier stored in the signed `oidc_csrf` cookie hashes to that
challenge (issue #318).

`oidc.rs::tests::claimable_only_when_passwordless_and_unbound` covers account
claiming (issue #319): a provisioned password-less, OIDC-unbound account is
claimed on first login, while password-bearing or already-bound accounts are
not.

OIDC group → role mapping (issue #322) is covered by
`core` `state::tests::resolves_group_to_org_role_grants` (declared `oidc_group`
lists resolve to `(organization, role)` grants) and
`oidc.rs::tests::collects_distinct_grants_for_presented_groups` (a user's group
claims collect the distinct grants applied additively on login).
## Unified resource access - `crate::access` and `crate::permissions`

All "load resource by name and check the caller may use it" logic lives in
two modules:

- `backend/core/src/permissions.rs` - declares the [`Permission`] capability
  enum (e.g. `EditProject`, `ManageMembers`, `ManageRoles`, `ManageWebhooks`),
  each capability's stable bit position in the `role.permission` bitmask
  (`Permission::bit`), and the canonical bitmasks for the three built-in
  roles (`admin_mask` / `write_mask` / `view_mask`). The mapping between
  roles and capabilities lives entirely in the database - `mask_grants`
  decides authorization from a role row's `permission` column, so custom
  roles configured at runtime require no code change at the call sites.
  The web crate re-exports this module as `web::permissions`.
- `backend/web/src/access.rs` - exposes `load_org`, `load_project`,
  `load_cache`, `load_webhook_in_org`, `load_integration_in_org`, plus the
  predicates `is_org_member` / `has_permission` and the new
  `load_membership_with_permissions` helper that loads the membership row
  alongside the role's permission bitmask in one logical step. Each loader
  takes an access policy enum (`OrgAccess`, `ProjectAccess`, `CacheAccess`)
  so handlers declare *what level of access they need* rather than stitching
  together ad-hoc lookup + permission + state-managed checks.

Unit tests in `access.rs` cover the role matrix and the managed-resource
guard:

- `org_admin_passes` - admin role + permission grants the resource.
- `org_admin_view_role_forbidden` - view role + admin-required permission →
  `WebError::Forbidden`.
- `org_admin_managed_forbidden` - state-managed org rejected for mutating
  permissions.
- `org_admin_non_member_not_found` - non-member → `WebError::NotFound`
  (no leak between "missing" and "not a member").
- `org_writable_write_role_passes` / `org_writable_view_role_forbidden` -
  write-tier permission honors Admin+Write but rejects View.
- `org_member_view_role_passes` - `OrgAccess::Member` accepts any role.
- `org_readable_public_visible_to_anon` /
  `org_readable_private_invisible_to_anon` - visibility rule for anonymous
  callers.
- `project_editable_admin_passes` / `project_editable_view_forbidden` /
  `project_editable_managed_forbidden` / `project_missing_returns_project_label` -
  same matrix at the project level, including the project-existence label
  guarantee.
- `cache_owned_unmanaged_passes` / `cache_editable_rejects_managed` /
  `cache_owned_allows_managed` / `cache_non_owner_returns_not_found` - cache
  matrix at the owner-scoped layer. `Editable` blocks state-managed caches
  (cache *config* is declarative), while `Owned` permits them so that NAR
  content endpoints can mutate operational data on managed caches.

Unit tests in `permissions.rs` (in `core`) lock the bitmask invariants:

- `each_permission_has_unique_bit` - no capability shares a bit position.
- `wire_names_round_trip` - every `Permission::as_wire_name()` parses back
  via `from_wire_name`.
- `admin_mask_grants_everything` - Admin's canonical mask covers
  `Permission::ALL`.
- `write_mask_excludes_admin_only_perms` - Write retains project/webhook
  management but cannot manage members, roles, or org settings.
- `view_mask_cannot_edit_projects_or_webhooks` - View is read-only on
  sensitive surfaces.
- `empty_mask_grants_nothing` - defensive: a role with `permission = 0`
  authorizes nothing.
- `mask_round_trips_through_vec` - `mask_to_vec` and `mask_from` are inverses.
- `view_org_is_not_mutating` / `is_builtin_role_recognises_seed_uuids`.

Run with: `cargo test -p web --lib access::tests`
and `cargo test -p core --lib permissions::tests`.

## Custom roles & role-management API

Issue #103 / #81 wired a DB-backed permission system: every role row carries
an `i64` bitmask in `role.permission`, capability authorization is a single
`mask & Permission::bit() != 0`, and a new `/orgs/{org}/roles` endpoint
family exposes CRUD for org-scoped custom roles.

Integration tests in `backend/web/tests/roles_crud.rs` exercise the full API
through `axum_test::TestServer` + `MockDatabase`:

- `list_roles_returns_builtins_plus_custom` - `GET /orgs/{org}/roles` is
  visible to any member and returns the three built-ins, all org-scoped
  custom roles, and an `available_permissions` catalogue derived from
  `Permission::ALL`.
- `create_role_persists_permission_bitmask` - `POST` resolves the wire-form
  permission identifiers, composes the bitmask, and round-trips it back on
  the response.
- `create_role_rejects_unknown_permission` - unknown identifiers return
  `400` with the offending name in the message.
- `create_role_rejects_view_role_caller` - only callers with the
  `manageRoles` permission may create roles; a View-tier member is rejected
  with `403`.
- `create_role_rejects_duplicate_name` - name uniqueness is scoped to
  `(org_id, name)` ∪ built-ins; duplicates return `409`.
- `patch_builtin_role_is_forbidden` / `delete_builtin_role_is_forbidden` -
  the three system roles are immutable.
- `patch_custom_role_updates_mask` - the bitmask is overwritten wholesale
  on PATCH so the UI can reflect a permission set without diff plumbing.
- `delete_role_in_use_is_rejected` - deleting a role that is still assigned
  to one or more members returns `400`; the caller must reassign first.
- `delete_unused_custom_role_succeeds` - the happy path.
- `custom_role_with_manage_webhooks_can_list_webhooks` /
  `custom_role_without_manage_webhooks_is_rejected` - end-to-end
  authorization through `access::has_permission`, proving that a custom
  role's bitmask reaches the webhook-list gate without any role-id
  comparison.

Run with: `cargo test -p web --test roles_crud`.

## Proto handshake - organization peer filtering

Helper `filter_org_peers_without_cache` runs during the `/proto` handshake's
`perform_auth` step. After token validation, each authorized peer that is an
organization is checked against the `organization_cache` table. Organizations
without a subscribed cache are moved into `failed_peers` with reason
`"organization has no cache subscribed"`. If the worker authenticated but the
authorized peer set ends up empty solely because of missing caches, the
connection is rejected with the dedicated `495 organization has no cache
subscribed` instead of a misleading `401`; a `401 no valid peer tokens provided`
is reserved for genuine token failures.

Backend tests (in `backend/proto/src/handler/auth.rs`):

- `tests::filter_org_peers_passes_through_org_with_cache`
- `tests::filter_org_peers_demotes_org_without_cache`
- `tests::filter_org_peers_passes_through_non_org_uuids`
- `tests::filter_org_peers_mixed`
- `tests::validate_then_filter_demotes_org_without_cache`

Auth-decision tests (in `backend/proto/src/handler/session.rs`):

- `auth_decision_tests::registered_but_no_valid_token`
- `auth_decision_tests::registered_emptied_by_missing_cache`

## Frontend - workers page no-cache banner

When the active organization has no subscribed cache, the workers page shows
a banner instructing the admin to subscribe to a cache before workers can run.

- `WorkersComponent - no-cache banner` - banner show/hide specs at
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

## CLI device authorization (`gradient login` web flow)

Integration tests in `backend/web/tests/cli_device_authorization.rs` pin the
state machine that backs `gradient login` (issue #251): a CLI calls
`POST /auth/cli/start`, polls `POST /auth/cli/poll`, and the browser-side user
hits `POST /auth/cli/authorize` or `/auth/cli/deny`.

Run with: `cargo test -p web --test cli_device_authorization`

| Test | Scenario | Expected |
|------|----------|----------|
| `start_returns_user_code_and_verification_uri` | `POST /auth/cli/start` | 200, response carries a `device_code`, dashed `user_code`, `verification_uri_complete` ending in `/account/cli-authorize?code=...`, and a positive `interval`/`expires_in` |
| `poll_pending_returns_cli_auth_pending` | poll on a row with no token/denial | 400, `code="cli_auth_pending"` |
| `poll_denied_returns_cli_auth_denied` | poll on a row with `denied_at` set | 400, `code="cli_auth_denied"` |
| `poll_expired_returns_cli_auth_expired` | poll on a row past `expires_at` | 400, `code="cli_auth_expired"` |
| `poll_authorized_returns_token_once` | poll on a row that has a token | 200, returns the session token |
| `poll_unknown_device_code_returns_404` | unknown `device_code` | 404 |
| `authorize_requires_auth` | `POST /auth/cli/authorize` without bearer | 403 |
| `deny_marks_row_denied` | authenticated `POST /auth/cli/deny` for a pending row | 200 |

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
- `maps_terminal_states` - `EvaluationStatus::{Completed, Failed, Aborted}` map to `CiStatus::{Success, Failure, Error}`.
- `skips_intermediate_states` - non-terminal statuses produce no CI status (avoids double-reporting `Running`).

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
- `narinfo_served_from_db_without_daemon_probe` - verifies the `.narinfo`
  response is assembled from DB rows (no nix-daemon probe) and now also asserts
  that the optional `Deriver:` line is emitted when `cached_path.deriver` is
  populated. Worker-supplied deriver metadata arrives via `NarUploaded.deriver`
  and is persisted in `mark_nar_stored`.
- `shows a friendly error when credentials are no longer available`

## Per-project `sign_cache` option (#125)

Backend:

- `cache::cacher::sign_sweep::tests::skip_when_all_producing_projects_private` -
  `compute_skipped_cached_paths` skips a path iff every producing project has
  `sign_cache=false` and at least one such project exists. A mixed
  public+private path stays signed (option B semantics).
- `cache::cacher::sign_sweep::tests::skip_set_empty_when_no_private_producers` -
  no skips when all producers are public.
- `web::tests::projects_sign_cache::get_project_includes_sign_cache` - GET
  `/api/v1/projects/{org}/{name}` returns `sign_cache` in the response body.
- `web::tests::projects_sign_cache::patch_project_writes_sign_cache_false` -
  PATCH with `{ "sign_cache": false }` is accepted and round-trips.
- `web::tests::projects_sign_cache::create_project_accepts_sign_cache_false` -
  PUT body may include `sign_cache: false`; default is `true` when omitted.
- `web::tests::narinfo::narinfo_returns_404_when_signature_null` -
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

The filter now uses `hash` - the same column the read path
(`get_nar_by_hash`) filters on - and updates **all** matching
`derivation_output` rows, also linking each row's `cached_path` UUID
to the freshly-written `cached_path` row.

## Worker NAR upload - store path normalisation

Eval-worker `get_derivation_path` returns drv paths as bare `<hash>-<name>.drv`
strings (no `/nix/store/` prefix). `nar::push_direct` and `nar::upload_presigned`
must canonicalise to the absolute path before handing it to harmonia's
`NarByteStream` (otherwise the NAR is empty → `NarSize: 0`) and before sending
`NarUploaded.store_path` to the server (otherwise `cached_path.store_path` is
stored without prefix and the served narinfo `StorePath:` line is malformed).

Backend (`cargo test -p worker --bins proto::nar::tests::ensure_full_store_path`):
- `ensure_full_store_path_prefixes_bare_hash_name` - bare drv path gets
  `/nix/store/` prepended.
- `ensure_full_store_path_preserves_absolute` - `/nix/store/...` paths are
  passed through unchanged.
- `ensure_full_store_path_preserves_other_absolute_paths` - unrelated absolute
  paths (e.g. test tmpdirs) are not touched.

## Worker NAR upload - fatal on incomplete path metadata

`push_direct` and `upload_presigned` used to swallow `gather_path_meta`
failures (`unwrap_or_default`) and ship `NarUploaded` with empty
`references`/`deriver`. The server's `mark_nar_stored` then persisted a
`cached_path` row with `references = NULL`, which the upload path never
revisits. A later build worker's prefetch closure walk relied on those
references to discover the `.drv`'s input_sources; with them missing the
daemon's `add_to_store_nar` parsed the `.drv` content, found the unstated
reference, and aborted with `path '…' is not valid`. Uploads must now fail
loudly so the cache is never seeded with an incomplete row.

Backend (`cargo test -p worker --bins proto::nar::tests`):
- `push_direct_fails_when_path_meta_unavailable` - with a store handle
  provided, a path `gather_path_meta` cannot resolve aborts the push with
  a "path metadata" error instead of emitting `NarUploaded` with empty
  metadata.
- `upload_presigned_fails_when_path_meta_unavailable` - same contract for
  the S3 / presigned-PUT branch.

## Worker prefetch - re-derive `.drv` references from content

When `prefetch_inputs` fetches a `.drv` during the closure walk, it harvests
seeds for the next iteration from the `.drv` content itself rather than
trusting `cached_path.references` alone. Under `ClosureMode::FollowOutputs`
(the default for input prefetch), outputs (so downstream builds find them),
input_derivations (transitive `.drv` prerequisites), and input_sources
(plain files the daemon validates as references when accepting the `.drv`
NAR) all enter the walk. This defends the prefetch against a `NULL`/stale
`cached_path.references` row.

`ensure_self_drv_present` uses `ClosureMode::InputsOnly` instead: when the
build target's own `.drv` is fetched, the walker must NOT pull declared
outputs, because those outputs are by definition not yet in the gradient
cache (the worker is about to build them). Including them would surface as
a fatal `Uncached` classification in `query_and_split` even though the
daemon only needs the input_derivation chain to accept the `.drv` import.
The omission is the fix for the cross-worker
`daemon add_to_store_nar … store path '…' does not exist` failures that
showed up when the build's `.drv` arrived without its input `.drv` chain.

Backend (`cargo test -p worker --bins proto::nar_import::tests`):
- `drv_closure_seeds_include_outputs_inputs_and_sources` - a synthetic
  `.drv` with one output, one input_derivation, and one input_source
  returns all three paths from `drv_closure_seeds` under
  `ClosureMode::FollowOutputs`.
- `drv_closure_seeds_inputs_only_excludes_outputs` - regression for the
  cross-worker `.drv` import failure: under `ClosureMode::InputsOnly` the
  declared output path is dropped while input_derivation + input_source
  remain.
- `drv_closure_seeds_skip_empty_output_paths` - content-addressed /
  deferred outputs (empty `path` field) are filtered so the closure walk
  never queries the empty string.

## Hash column normalization (file_hash / nar_hash)

The `derivation_output.file_hash`, `cached_path.file_hash`, and
`cached_path.nar_hash` columns are persisted in the canonical
`{algo}:<nix32>` form (`sha256` for new uploads; `blake3` for rows
written while issue #132's BLAKE3 default was active) so the URL hash
extracted from a narinfo `URL:` field matches the column directly.
Workers send the prefixed hash over the wire; the proto handler and
scheduler call `gradient_core::nix_hash::normalize_nar_hash` before
`Set(...)`. Migration `m20260430_000000_normalize_hash_columns` backfills
pre-existing rows.

Backend:
- `cargo test -p core --lib nix_hash` - round-trip and idempotency tests for
  `normalize_nar_hash` covering SRI, prefixed hex, prefixed nix32, bare hex,
  rejection of malformed inputs, and the BLAKE3 variants of each (test
  vectors cross-checked against NixOS/nix PR #12379). Also covers
  `strip_hash_algo`, the algorithm-agnostic prefix stripper used when
  building narinfo `URL:` slugs.
- `cargo test -p migration --lib normalize_hash_columns` - covers the
  hex→nix32 conversion helper used by the backfill migration.
- `cargo test -p web --lib endpoints::caches::nar::tests` -
  `resolve_returns_store_hash_for_normalized_derivation_output` is the
  regression test for the original 404 bug: a narinfo URL hash (nix32)
  resolves a `derivation_output` row whose `file_hash` is in canonical
  `sha256:<nix32>` form.

## NAR path extraction - file or directory subtree

`core::storage::nar_extract::extract_path_from_nar_bytes` returns either
`Extracted::File` (regular file body) or `Extracted::Directory { tar_zst }`
(zstd-compressed tar of the matched subtree). The download endpoints
(`/builds/{build}/download/{filename}` and the project-level entry-point
download) detect the variant and set `Content-Type: application/zstd` plus a
`.tar.zst`-suffixed `Content-Disposition` filename for the directory case.

Backend (`cargo test -p core --test nar_extract`):
- `extracts_file_at_relative_path`, `extracts_file_in_nested_directory`,
  `drains_non_matching_sibling_before_extracting_target`,
  `returns_not_found_for_missing_path` - file-mode behaviours preserved.
- `extracts_directory_as_tar_zst` - regression for "fails if build output is
  a folder": when the build product's relative path resolves to a directory
  in the NAR, the extractor walks the subtree, emits tar entries for nested
  directories and files (preserving the executable bit), and zstd-compresses
  the result.
- `directory_tarball_preserves_symlinks` - symlinks inside the matched
  subtree are written as `tar::EntryType::Symlink` with the original target
  bytes, not flattened to regular files.
- `directory_match_at_root_via_basename` - a build product whose path equals
  the output store path returns the whole subtree as `tar.zst`, with entries
  rooted at the matched directory's basename so extraction recreates that
  name.

## Upstream narinfo metadata for worker prefetch

Backend (`cargo test -p proto --lib handler::cache::tests`):
- `parse_upstream_narinfo_full_fields` - verifies the server parses
  `NarHash`, `NarSize`, `FileSize`, `References`, `Deriver`, and `Sig` from an
  upstream `.narinfo` body so the worker receives enough metadata to build a
  `ValidPathInfo` and call `add_to_store_nar`. Without this the worker
  silently failed imports and the build died with
  "dependency does not exist, and substitution is disabled".
- `parse_upstream_narinfo_requires_url` - a narinfo without `URL:` is rejected.
- `parse_upstream_narinfo_trims_base_url_trailing_slash` - joins
  `base_url` + `URL:` without double slashes.
- `parse_upstream_narinfo_empty_references_is_some_empty` - `References:` with
  no paths yields `Some(vec![])`, not `None`.
- `parse_upstream_narinfo_ignores_unparseable_sizes` - malformed `NarSize` /
  `FileSize` fall back to `None` rather than aborting the parse.

## Worker prefetch robustness - uncached inputs and broken daemon connections

Backend (`cargo test -p worker --tests`):
- `nix::store::tests::scoped_guard_discards_inner_when_not_marked_ok` -
  every daemon op in the worker runs against a `ScopedGuard`. If the guard
  is dropped without an explicit `mark_ok()` call - the path taken on `Err`
  returns, panics, and `await` cancellation (e.g. `FuturesUnordered` being
  dropped on the first prefetch import failure) - `Drop` discards the
  pooled connection so the next acquirer doesn't inherit a possibly
  out-of-phase protocol stream. Without this, a cancelled `add_to_store_nar`
  would silently recycle a mid-frame connection and the next caller would
  surface as `"serialised integer N is too large for type 'j'"` or as
  `query_path_info` returning `Ok(None)` on a path that exists.
- `nix::store::tests::scoped_guard_preserves_inner_when_marked_ok` - the
  symmetric success path: a daemon op that completes cleanly calls
  `mark_ok()` and the connection is recycled, preserving pool warmth.
- `proto::nar_import::tests::classify_splits_cached_by_url_presence` - cached
  entries with a presigned `download_url` go to the S3 bucket, those without
  go to the WebSocket `NarRequest` bucket.
- `proto::nar_import::tests::classify_collects_uncached_separately` -
  regression guard for the Stage-3 prefetch hard-fail: when the server
  reports a required input as `Uncached`, it is *not* silently skipped.
  Previously the path was dropped on the floor and a dependent build
  eventually failed inside `add_to_store_nar` with
  `path '/nix/store/…' is not valid`; classifying it explicitly lets the
  prefetcher abort with a clear message that names the missing path.
- `proto::nar_import::tests::classify_empty_input_is_empty_output` - empty
  cache responses produce empty buckets.

## State configuration - optional fields for OIDC-only users

Backend (`cargo test -p core --lib state::tests`):
- `user_accepts_missing_password_file` - `StateUser` accepts a JSON
  document with `"password_file": null`, so the NixOS module may emit
  OIDC-only users without a password credential file.
- `org_project_cache_descriptions_optional` - `description` on
  organizations, projects, and caches is optional; a full config without
  them validates cleanly.
- `state_project_accepts_wildcard_field` - `StateProject` deserialises
  the canonical `wildcard` field.
- `state_project_accepts_legacy_evaluation_wildcard_alias` - pre-rename
  state files using `evaluation_wildcard` continue to parse via the
  serde alias, so existing `gradient-state.nix` configurations don't
  break on upgrade.
- `state_project_keep_evaluations_defaults_to_thirty` - when a project
  omits `keep_evaluations`, the parsed value defaults to 30 so newly
  provisioned state-managed projects match the API-created default and
  the GC pass keeps a meaningful history window.
- `state_project_keep_evaluations_zero_rejected_by_validator` - the
  configuration validator rejects `keep_evaluations < 1`, matching the
  `types.ints.positive` constraint in `nix/modules/gradient-state.nix`
  and the API-level `apply_keep_evaluations` check.
- `default_keep_evaluations_is_thirty`
  (`backend/core/src/types/cli/storage.rs`) - `StorageArgs::default()`
  yields `keep_evaluations = 30` so a default `gradient-server` install
  bounds per-project evaluation retention instead of allowing unbounded
  growth (issue #92).
- `default_nar_ttl_hours_is_two_weeks`
  (`backend/core/src/types/cli/storage.rs`) - `StorageArgs::default()`
  yields `nar_ttl_hours = 336` (2 weeks) so cached NARs eventually
  expire on a default deploy (issue #92).
- `clap_default_keep_evaluations_is_thirty`
  (`backend/core/src/types/cli/storage.rs`) - parsing an empty argv
  through clap yields `keep_evaluations = 30`, guarding against drift
  between the `#[arg(default_value …)]` attribute and the `Default`
  impl.
- `clap_default_nar_ttl_hours_is_two_weeks`
  (`backend/core/src/types/cli/storage.rs`) - parsing an empty argv
  through clap yields `nar_ttl_hours = 336`, same drift guard.
- `state_project_silently_ignores_legacy_force_evaluation_field` -
  state files written before the rename may still carry
  `force_evaluation`; serde's default unknown-field handling drops it
  silently so existing deployments parse cleanly after the field's
  removal from the schema.
- `state_org_members_serde_round_trip` - `StateOrganization.members`
  round-trips through JSON as `[{ user, role }]` entries, covering both
  built-in (`Write`) and custom (`releaser`) role names.
- `state_org_members_default_empty` - omitting `members` deserialises
  to an empty `Vec`, preserving the legacy creator-as-Admin behavior on
  state files that predate the field (issue #94).
- `state_org_members_validator_accepts_builtin_role` - a member with
  `role = "Write"` validates when the referenced user exists.
- `state_org_members_validator_accepts_custom_org_role` - a member can
  reference a state-managed custom org role declared under
  `state.roles` scoped to the same organization.
- `state_org_members_validator_rejects_unknown_role` - a role name
  that is neither a built-in nor a declared org role yields a
  validation error pinpointing
  `organizations.<org>.members.<user>.role`.
- `state_org_members_validator_ignores_unknown_user` - a member
  referencing a user that does not exist passes validation; the
  membership is deferred and applied on registration / OIDC first-login
  (issue #94 contract).
- `state_org_members_validator_rejects_duplicate_user` - two
  member entries with the same `user` in one org's `members` is an
  error.
- `pending_membership_tests::apply_pending_returns_zero_for_unknown_user`
  (`backend/core/src/state/provisioning.rs`) -
  `apply_pending_org_memberships` is a no-op when the username has no
  pending entries; callable from any user-creation path without a
  matching state declaration.
- `keep_set_tests::keep_sets_track_inner_name_not_attrset_key`
  (`backend/core/src/state/provisioning.rs`) - `gradient-state.nix`
  exposes `name = mkOption { default = <attrset key>; }` on users,
  organizations, projects, caches, and API keys, so a user may pin
  `projects.foo = { name = "main"; … }`. Every `apply_*` writes the
  override value to the DB row; `unmark_removed_entities` therefore must
  also key its keep-sets on the value's `name`/`username` field, not on
  the HashMap key, or the cleanup pass deletes (or unmarks) the row the
  same reconciliation just inserted.

These pin the wire contract between `nix/modules/gradient-state.nix`
(`types.nullOr types.str` on `password_file` and the three `description`
options) and `backend/core/src/state/mod.rs`. Without them, provisioning a
user intended for OIDC failed at startup with "missing field
`password_file`", and the user's subsequent OIDC login was rejected by
`web::authorization::oidc` with `User already exists with password
authentication`.

## Reporter trigger rejects outbound integration (#326)

Backend (`cargo test -p core --lib state::provisioning::trigger_helper_tests`):
- `build_reporter_pr_rejects_outbound_integration_with_kind_aware_error` -
  a `reporter_pull_request` / `reporter_push` trigger referencing a name that
  exists only as an `outbound` integration now fails with a kind-aware error
  naming both `inbound` and `outbound`, instead of the misleading
  `unknown integration` raised when the name genuinely doesn't exist. Reporter
  triggers resolve against inbound integrations only (the webhook resolver
  matches the inbound id), so pointing one at an outbound integration would
  otherwise persist an id no webhook ever resolves to and the trigger would
  silently never fire.

## Scheduler does not double-dispatch a build

Backend (`cargo test -p scheduler --lib jobs::tests::add_pending_does_not_requeue_active_job`):
- `add_pending_does_not_requeue_active_job` - once a build is assigned (moved to
  `active`), re-adding the same job id must not put it back in `pending`. Two
  concurrent `dispatch_ready_builds` passes can both clear the `contains_job`
  filter before either enqueues; without the idempotency guard the same build is
  dispatched to the worker twice, the duplicate is aborted by the nix daemon
  ("build aborted by server"), and the spurious failure fails the whole
  evaluation (observed flaking the `gradient-cache` NixOS VM test).

## Org members can view subscribed caches (#327)

NixOS VM (`nix/tests/gradient/state`):
- `org member can view subscribed caches they do not own` - a state-provisioned
  cache appears in `GET /caches` for any member of a subscribed organization,
  so viewing it by name (`GET /caches/{cache}`, `gradient cache show`) must
  succeed too. `bob` (a plain `corp` member who created none of the caches and
  holds no cache membership) lists and shows both the active `main` and inactive
  `dev` caches. Regression: detail lookups previously required a direct
  `cache_user` row, so org members got a 404 via the API, CLI and WebUI.
- `non-members cannot see private caches` - `charlie`, who belongs to no
  subscribing org, neither lists nor can show the private caches, pinning the
  broadened read access so it does not leak caches to unrelated users.

## Hashed API keys at rest

Backend (`cargo test -p core --lib state::provisioning::api_key_hash_tests`):
- `accepts_64_char_hex` - a lowercase 64-char hex string round-trips.
- `trims_trailing_whitespace` - credential files written with a trailing
  newline still parse.
- `lowercases_uppercase_hex` - uppercase hex is normalised on the way in.
- `rejects_plaintext_token` / `rejects_short_hex` / `rejects_non_hex_chars` -
  malformed values are rejected with a "SHA-256" hint pointing at the right
  shell incantation.

Backend (`cargo test -p core --lib state::provisioning::helper_tests`):
- `lookup_id_returns_id_when_present` / `lookup_id_errors_with_kind_and_name` -
  pin the shared `lookup_id` helper used by every `apply_*` provisioning step
  so missing user/org references produce a uniform `"<Kind> '<name>' not
  found"` error.
- `read_credential_default_dir_when_env_unset` /
  `credentials_dir_returns_nonempty` - pin the shared credential-file
  resolver: the error always names the file it tried to read and the file
  label, so an operator who misnames a systemd credential sees which one.

Backend (`cargo test -p migration --lib m20260502_000000_hash_api_keys`):
- `sha256_hex_known_vector` - pins the in-place migration's digest helper to
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
`cargo test --workspace --tests` - any `entity::build::Model` literal
that forgot the new field would fail to compile.

## Evaluation builds list - follower → leader substitution

`GET /evals/{evaluation}/builds` is the build list rendered alongside logs on
the evaluation page. A follower build (`build.via` set) is a placeholder waiting
on a leader build in another evaluation that's doing the actual work - until
`propagate_to_followers` runs, the follower's `status`, `updated_at`,
`build_time_ms` and `log_id` are stale stand-ins. The handler resolves
followers to their leader row so the client sees the live build it can act on
(log endpoint, downloads, dependency graph all key off `id`).

Tests (`backend/web/tests/evaluation_builds_via.rs`):
- `follower_build_is_replaced_with_leader_row` - a follower with
  `via=Some(leader_id)` produces a list item carrying the leader's `id`,
  `status`, and `updated_at`; the shared derivation path keeps `name`
  unchanged.
- `plain_build_returns_own_row_without_extra_query` - a build with `via=None`
  short-circuits the leader-resolution query (no extra `SELECT builds`) and
  returns its own row.

## Evaluation artefact tree

`GET /evals/{evaluation}/artefacts` returns a nested tree (entry point →
output → product) consumed by the `gradient download` CLI artefact picker.
The handler issues a fixed number of batched queries - one per
`entry_point` / `build` / `derivation` / `derivation_output` / `build_product`
table - and buckets the rows in memory rather than N+1 per derivation.

Tests (`backend/web/tests/evals_artefacts.rs`):
- `empty_eval_returns_empty_tree` - evaluation with zero entry points
  short-circuits to `entry_points: []` after the eval-access load.
- `returns_full_tree_grouped_by_entry_point_and_output` - seeds one entry
  point with two derivation outputs and three products; asserts grouping,
  alphabetic ordering of outputs by `name` and products by `path`, the
  `type`/`subtype`/`id` serde rename mapping for `build_product.file_type`,
  and that `entry_point.build_id` is populated (used by the CLI download
  picker to resolve `/builds/{build}/download/{filename}` without a second
  lookup).
- `missing_eval_returns_404` - non-existent evaluation id surfaces as
  `404 Not Found`.
- `public_org_allows_anonymous` - anonymous request against an evaluation
  owned by a `public=true` organization succeeds without a Bearer token.
- `private_org_rejects_anonymous` - anonymous request against a
  `public=false` organization returns `404` (the same shape eval-access uses
  to avoid distinguishing missing from forbidden).

## EvalMessage - worker-surfaced evaluation messages

Backend (`cargo test -p scheduler --tests scheduler_tests::record_eval_message`):
- `record_eval_message_drops_when_job_unknown` - a `ClientMessage::EvalMessage`
  whose `job_id` is not an active scheduler job is silently accepted (no DB
  insert, no error). Ensures stale messages from finished jobs can't poison
  the evaluation log.
- `record_eval_message_inserts_for_active_build_job` - for an enqueued build
  job the handler resolves `PendingJob::evaluation_id()` and inserts one row
  into `evaluation_message`. Build compile failures and user-initiated aborts
  deliberately do not flow through this path.

## Cache GC - orphan files keep predicate

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

- `cacher::cleanup::tests::keeps_active_drops_orphan` - file for an active
  build's hash survives; file with no DB references is removed.
- `cacher::cleanup::tests::keeps_cached_path_only` - a hash returned only by
  the `cached_path.file_hash IS NOT NULL` UNION branch is kept (covers `.drv`
  files that have no `derivation_output`).
- `cacher::cleanup::tests::drops_everything_when_no_keep` - empty keep set
  removes every on-disk NAR.

## Cache GC - TTL pass orphan guard

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

- `cacher::cleanup::tests::stale_nars_disabled_when_ttl_zero` - pass is a
  no-op when `nar_ttl_hours = 0`.
- `cacher::cleanup::tests::stale_nars_no_eligible_rows` - empty SELECT
  result leaves on-disk NARs untouched.
- `cacher::cleanup::tests::ttl_select_skips_fixed_output_derivations` -
  regression for #107: the TTL SELECT must keep its `derivation_output.ca
  IS NOT NULL` guard so FOD NARs are never evicted by the TTL pass.

## Per-project evaluation GC retention policy (#305)

`evaluations_to_gc` (`backend/core/src/db/gc.rs`) decides, by index into a
newest-first evaluation list, which evaluations `gc_project_evaluations`
deletes for a project's `keep_evaluations` count. Active evaluations
(Queued/Fetching/Evaluating*/Building/Waiting) are never deleted and never
consume a `keep` slot; among terminal evaluations the `keep` most recent
`Completed`/`Failed` ("done") are retained, and `Aborted` evaluations are
retained only to fill remaining slots when too few done evaluations exist.
This fixes #305 where a building/queued evaluation consumed the single keep
slot and the last successful evaluation was deleted.

- `core::db::gc::tests::keeps_last_done_when_newer_evaluation_is_active` -
  `keep = 1` with a newer active evaluation deletes nothing.
- `core::db::gc::tests::never_deletes_active_evaluations` - an all-active
  list is never touched regardless of `keep`.
- `core::db::gc::tests::gcs_aborted_when_a_done_evaluation_exists` - a newer
  `Aborted` is deleted in favour of an older `Completed`.
- `core::db::gc::tests::keeps_aborted_when_no_done_evaluation_exists` - a
  lone `Aborted` is retained when no done evaluation exists.
- `core::db::gc::tests::done_evaluations_take_priority_over_aborted` - done
  evaluations fill `keep` slots ahead of `Aborted` ones.
- `core::db::gc::tests::deletes_done_evaluations_beyond_keep` - done
  evaluations past `keep` are deleted.
- `core::db::gc::tests::active_evaluations_do_not_consume_keep_slots` - an
  active evaluation does not occupy a slot, so the newest done evaluation
  is retained.
- `core::db::gc::tests::keep_zero_deletes_nothing` - `keep = 0` is a no-op.

## Frontend - form primitives & style guide

Reusable form primitives live under
`frontend/src/app/shared/components/form/` and consolidate the
label + input + error + dialog + message-banner patterns previously
duplicated across feature components. A `/styleguide` route at
`frontend/src/app/features/styleguide/` exercises every primitive and
serves as a living reference.

Specs (vitest + jsdom):

- `FormFieldComponent` - renders label/required marker; toggles
  `has-error` class on touched + invalid control.
  (`shared/components/form/form-field/form-field.component.spec.ts`)
- `FormErrorComponent` - hidden until touched; resolves default
  messages by error key; honours overrides; formats `minlength` with
  required length.
  (`shared/components/form/form-error/form-error.component.spec.ts`)
- `MessageBannerComponent` - applies `--type` modifier class; uses
  default icon per type; honours custom icon override.
  (`shared/components/form/message-banner/message-banner.component.spec.ts`)
- `PasswordInputComponent` - toggles input type between `password`
  and `text` on the eye button.
  (`shared/components/form/password-input/password-input.component.spec.ts`)
- `FormFieldsBuilder` - typed wrappers for text/email/password/confirm
  produce controls with the expected validators; password strength
  validator covers length + character class requirements; cross-field
  `confirm()` validates against the named control.
  (`shared/components/form/form-fields-builder.spec.ts`)
- `HeaderComponent` - Register link is rendered when registration is
  enabled and hidden when `ConfigService.registrationDisabled` is true,
  matching the server `/config` response. Direct regression for #218.
  (`shared/components/header/header.component.spec.ts`)

## CI check names - org/project context

CI check names reported to GitHub/Gitea now include the organization
and project so multiple Gradient instances/projects sharing a forge
repository remain distinguishable. Helpers live in
`backend/core/src/ci/reporting.rs` and are reused by the
`ForgeStatusReport` action dispatcher (`backend/core/src/ci/actions.rs`):

- Evaluation roll-up: `Gradient Evaluation {org}/{project}` (e.g.
  `Gradient Evaluation wavelens/my-project`).
- Per-entry-point build: `Gradient Build {org}/{project}: {entry_point}`.
- When the organization lookup returns `None`, the scope degrades to
  just `{project}`.

Tests (`cargo test -p core --tests ci::reporting`):

- `check_scope_with_org` - `Some("wavelens"), "my-project"` →
  `"wavelens/my-project"`.
- `check_scope_without_org_falls_back_to_project` - `None, "my-project"`
  → `"my-project"`.
- `evaluation_context_format` - produces the new
  `"Gradient Evaluation …"` string.
- `build_context_format` - produces
  `"Gradient Build wavelens/my-project: my-package"`.
- `build_context_falls_back_when_org_missing` - degrades correctly when
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

- `auth_tier_throttles_burst` - 5 successive `POST /api/v1/auth/check-username`
  requests succeed, 6th returns `429`.
- `cache_tier_does_not_throttle_moderate_burst` - 50 successive GETs to
  `/cache/{cache}/nix-cache-info` never return `429`.

## Outgoing webhook URL - SSRF validation

`validate_webhook_url` (in `backend/core/src/ci/webhook.rs`) is the gate
between user-supplied webhook URLs and the outbound HTTP client. It is
called at create/update time (in `web::endpoints::webhooks::{put,
patch_webhook}`) and again at delivery time inside
`ReqwestWebhookClient::deliver`, which also performs DNS resolution and
rejects any resolved IP in a disallowed range. Redirects are disabled on
the production reqwest client.

Unit tests (`cargo test -p core --tests ci::webhook`):

- `validate_url_accepts_public_https` - `https://`/`http://` to public
  hostnames pass.
- `validate_url_rejects_invalid_scheme` - `file://`, `ftp://`,
  `gopher://`, `javascript:` are rejected.
- `validate_url_rejects_unparseable` - empty / non-URL strings rejected.
- `validate_url_rejects_localhost_name` - `localhost` (any case) is
  rejected.
- `validate_url_rejects_loopback_ipv4` - `127.0.0.0/8` blocked.
- `validate_url_rejects_aws_metadata_ip` - covers the motivating attack
  (`169.254.169.254`) plus the wider link-local block.
- `validate_url_rejects_rfc1918_ranges` - `10.x`, `172.16-31.x`,
  `192.168.x`.
- `validate_url_rejects_cgnat_shared_space` - `100.64.0.0/10` blocked,
  with boundary asserts that adjacent public space (`100.63.255.255`,
  `100.128.0.1`) is allowed.
- `validate_url_rejects_unspecified_and_broadcast` - `0.0.0.0`,
  `255.255.255.255`.
- `validate_url_rejects_multicast_ipv4` - `224.0.0.0/4`.
- `validate_url_rejects_reserved_ipv4` - `240.0.0.0/4`.
- `validate_url_rejects_ipv6_loopback_and_unspecified` - `::1`, `::`.
- `validate_url_rejects_ipv6_link_and_unique_local` - `fe80::/10`,
  `fc00::/7`.
- `validate_url_rejects_ipv6_multicast` - `ff00::/8`.
- `validate_url_rejects_ipv4_mapped_loopback_in_ipv6` - `::ffff:127.0.0.1`
  and `::ffff:169.254.169.254` blocked via the embedded-v4 check.
- `validate_url_accepts_public_ipv4_literal` /
  `validate_url_accepts_public_ipv6_literal` - sanity asserts that
  legitimate public IP literals (`8.8.8.8`, `2001:4860:4860::8888`) pass.

## CI reporter base URL - SSRF + redirect token leak (#113)

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
  `github_app_reporter_rejects_aws_metadata_ip` - the motivating
  attack (`169.254.169.254`) is rejected by all three constructors.
- `gitea_reporter_rejects_localhost_hostname` /
  `github_reporter_rejects_localhost_hostname` - literal `localhost`
  rejected.
- `gitea_reporter_rejects_loopback_ipv4` /
  `github_reporter_rejects_ipv6_loopback` - `127.0.0.1`, `[::1]`
  rejected.
- `gitea_reporter_rejects_rfc1918` - `10.x`, `192.168.x` rejected.
- `gitea_reporter_rejects_non_http_scheme` - `file://`, `ftp://`
  rejected.
- `github_app_reporter_empty_url_still_uses_default` - empty string
  continues to fall back to `https://api.github.com` (the field is
  optional in `integration_lookup`).
- `reporter_for_project_unsafe_url_falls_back_to_noop` - an unsafe
  Gitea base URL plumbed through the factory degrades to
  `NoopCiReporter` rather than crashing the caller.

## GitLab outbound CI reporter (#90)

`GitlabReporter` (in `backend/core/src/ci/reporter.rs`) posts commit
statuses to GitLab via `POST {base_url}/api/v4/projects/{id}/statuses/{sha}`,
where `id` is the URL-encoded `owner/repo` path (also covers nested
groups such as `group/sub/repo`). Authenticates with `PRIVATE-TOKEN`,
which accepts personal, project, and group access tokens. The
`ForgeStatusReport` action dispatcher in `backend/core/src/ci/actions.rs`
resolves the integration row and constructs a `GitlabReporter` (or the
appropriate forge-specific reporter) per dispatch — the legacy per-project
lookup helper has been removed.

Unit tests (`cargo test -p core --tests ci::reporter`):

- `gitlab_state_from_ci_status_all_variants` - every `CiStatus` maps
  to the documented GitLab state (`pending`, `running`, `success`,
  `failed`, with `Error` collapsed to `failed`).
- `gitlab_state_serializes_lowercase` - wire format matches the
  GitLab API enum.
- `gitlab_project_id_flat_path` /
  `gitlab_project_id_nested_groups` - `owner/repo` is URL-encoded as
  `acme%2Fwidgets`, and nested groups (`group/sub/repo`) become
  `group%2Fsub%2Frepo`.
- `gitlab_reporter_trims_trailing_slash` - base URL normalised.
- `gitlab_reporter_rejects_aws_metadata_ip` /
  `gitlab_reporter_rejects_localhost_hostname` /
  `gitlab_reporter_rejects_non_http_scheme` - same SSRF gate as the
  other reporters (`169.254.169.254`, `localhost`, `file://`).
- `reporter_for_project_gitlab_builds_gitlab` - the public factory
  builds a `GitlabReporter` for `ci_type="gitlab"`.

## SSH private key decryption - no plaintext fallback

`decrypt_ssh_private_key` in `backend/core/src/sources/ssh_key.rs`
decrypts the per-organization SSH key from `organization.private_key`.
Decryption failure must NOT silently fall back to interpreting the
stored value as a plaintext PEM, otherwise anyone with write access to
that column could bypass encryption entirely.

Tests (`backend/core/src/sources/ssh_key.rs`):

- `decrypt_ssh_key_corrupt_base64_fails` - non-base64 column rejected
  with `OrganizationKeyDecoding`.
- `decrypt_ssh_key_plaintext_pem_rejected` - a base64-encoded plaintext
  OpenSSH PEM placed directly in the column is rejected with
  `KeyDecryption`, not accepted.
- `decrypt_ssh_key_plaintext_non_pem_rejected` - random base64 garbage
  also fails with `KeyDecryption`.
- `generate_ssh_key_decrypts_to_openssh_pem` - properly encrypted keys
  still round-trip through decrypt.
## Body-size limits - webhook and blob upload (#51)

Without a body-size cap, `field.bytes().await` and the `body: Bytes`
extractor used by `forge_hooks` would buffer entire request bodies into
memory, allowing a single 10 GB payload to OOM the server.
`create_router` (`backend/web/src/lib.rs`) now applies an
`axum::extract::DefaultBodyLimit::max(cli.max_request_size)` layer to the
whole API router (default 2 MiB) and overrides it on
`POST /api/v1/build-requests/{session}/blobs` with the fixed
`MAX_BUILD_REQUEST_SIZE` (20 MiB) for blob uploads.

Tests (`cargo test -p web --test body_size_limit`):

- `webhook_body_over_limit_returns_413` - a 4 KiB POST to
  `/api/v1/hooks/github` with `max_request_size = 1024` is rejected with
  `413 Payload Too Large` *before* the handler runs (so the OOM-prone
  `body: Bytes` read never happens).
- `webhook_body_within_limit_reaches_handler` - a 256 B body under the
  same 1 KiB cap is *not* short-circuited with 413; the handler runs and
  returns its normal response.
- `blob_upload_route_uses_higher_limit` - a 16 KiB body to
  `POST /api/v1/build-requests/{session}/blobs` with
  `max_request_size = 1024` is *not* rejected with 413, proving the
  per-route override to `MAX_BUILD_REQUEST_SIZE` is wired up.

Regression for the build-request rework
(`cargo test -p web --test old_direct_build_gone`):

- `post_builds_returns_404` and `get_recent_direct_builds_returns_404` -
  the legacy `POST /api/v1/builds` and `GET /api/v1/builds/direct/recent`
  routes are no longer registered.

## Build request rework (#234)

The new `gradient build` flow uploads git-tracked files via a three-step
content-addressed pipeline (`manifest` → `blobs` → `dispatch`) and
materialises `/nix/store/<hash>-source` server-side. The legacy
`direct_build` table and its endpoints are gone in a clean break (no data
migration). Tests:

- `backend/web/tests/build_requests_manifest.rs` - manifest validation:
  path syntax checks (`.` / `..` / `/absolute` / null bytes / duplicates),
  per-file hex hash decoding, total-size cap (`MAX_BUILD_REQUEST_SIZE`,
  20 MiB → 413), and happy-path session creation that surfaces the
  missing-blob hex list.
- `backend/web/tests/build_requests_blobs.rs` - multipart blob upload:
  hash-mismatch and foreign-hash rejection (both 400), already-dispatched
  session (409), expired session (410), session-not-found (404), and the
  happy path verifying both `session.missing` is cleared in the DB and a
  `build_request_blob` row exists.
- `backend/web/tests/build_requests_dispatch.rs` - dispatch flow guards:
  blobs-still-missing → 409, double-dispatch → 409, expired → 410,
  session-not-found → 404, plus a smoke happy-path on the empty-manifest
  branch (real-blob coverage runs in CI against Postgres).
- `backend/web/tests/evals_artefacts.rs` - artefact tree response: empty
  evaluation returns empty `entry_points`, full tree exposes
  `entry_point → outputs → products` with alphabetic ordering and the
  `build_id` field on each entry point, public-org evaluations allow
  anonymous access, private-org evaluations 404 for anonymous callers.
- `backend/web/tests/old_direct_build_gone.rs` - see the regression
  block above.
- `backend/core/src/storage/source_nar.rs` - in-file unit tests for
  `materialise_source_nar`: deterministic NAR hash + store path across
  repeat calls, `-source` suffix on the resulting `/nix/store/<hash>`,
  and the canonical 32-char base32 hash shape.
- `backend/cache/src/cacher/cleanup.rs` - GC sweeps for the new tables:
  `build_request_blob_sweep_evicts_stale` and
  `build_request_blob_sweep_disabled_when_ttl_zero` cover the blob TTL
  (driven by the existing `nar_ttl_hours` global), and
  `upload_session_sweep_deletes_expired_undispatched` proves that
  expired sessions without a `dispatched_at` flip are reclaimed.

## Cache traffic metrics - atomic UPSERT (no lost updates)

`record_nar_traffic` (`backend/web/src/endpoints/stats.rs`) records bytes
served per `(cache, bucket_time)` row. The previous implementation used a
SELECT-then-UPDATE/INSERT pattern, which dropped updates whenever two NAR
fetches in the same minute bucket ran concurrently - both reads observed
the same `bytes_sent` value and the second writer clobbered the first
(see issue #50). It is now a single `INSERT … ON CONFLICT (cache,
bucket_time) DO UPDATE SET bytes_sent = bytes_sent + EXCLUDED.bytes_sent,
nar_count = nar_count + EXCLUDED.nar_count`, which Postgres serialises on
the unique index so every caller's increment is preserved.

Tests (`cargo test -p web --lib stats`):

- `record_nar_traffic_stmt_is_atomic_upsert` - asserts the generated SQL
  contains `INSERT INTO cache_metric`, `ON CONFLICT (cache, bucket_time)`,
  the additive `bytes_sent`/`nar_count` updates, and contains no `SELECT`
  (a `SELECT` would reintroduce the read-modify-write race).
## Worker-peer token verification - argon2 + constant time

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

- `validate_tokens_argon2_hash_authorizes` - argon2-hashed registration
  authorises the matching plaintext token.
- `validate_tokens_argon2_wrong_token_fails` - argon2 row rejects
  wrong tokens with `"invalid token"`.
- `verify_token_dispatches_on_format` - `$argon2…` routes to
  `password_auth`; lowercase hex routes to constant-time SHA-256.
- The pre-existing `validate_tokens_*` tests using `sha256_hex` continue
  to cover the legacy-format compatibility path.

## Sign sweep - batched, bounded, single crypt-secret read (#105)

`sign_missing_signatures` (in `backend/cache/src/cacher/sign_sweep.rs`)
used to issue 2 SELECTs per pending row (cache + cached_path), reload
the crypt-secret file from disk, and re-decrypt each cache's private
key on every row, with no `LIMIT` on the initial query - at scale this
became 50k+ DB calls plus 50k+ crypt-secret reads per minute, and a
single backlog could pin one DB connection indefinitely.

The sweep is now `LIMIT`-bounded (`SIGN_SWEEP_BATCH = 1000` rows per
pass) and batches the `cache` / `cached_path` lookups into one
`is_in(...)` query each. Per-cache decrypted keys are wrapped in a new
`CacheSigner` (in `backend/core/src/sources/cache_key.rs`) built once
per pass per cache - the crypt secret is read at most once per cache,
not once per signature. `sign_narinfo_fingerprint` is now a thin
one-shot wrapper around `CacheSigner::sign_narinfo` so existing
callers keep working byte-for-byte.

Unit tests (`cargo test -p core --lib sources::cache_key`):

- `cache_signer_matches_one_shot_signer` - for several
  `(store_path, nar_hash, nar_size, refs)` tuples, asserts that the
  signature produced by `CacheSigner::sign_narinfo` is byte-identical
  to the one produced by `sign_narinfo_fingerprint`. Guards against the
  batching refactor silently changing the on-wire fingerprint.
- `cache_signer_rejects_bad_key_at_build_time` - a cache row whose
  `private_key` cannot be base64-decoded fails at
  `CacheSigner::from_cache`, so the sweep can mark the cache as
  unsignable for the rest of the pass instead of repeating the
  decryption error per row.

After issue #132, the dedicated `hex_hash_to_nix32` helper was removed
and `sign_missing_signatures` calls `gradient_core::nix_hash::normalize_nar_hash`
directly. The hash-format conversion path is now covered by the
algorithm-aware test suite in `cargo test -p core --lib nix_hash` (see
above), which exercises both `sha256:` and `blake3:` inputs.

## Proto WebSocket - message-size cap & handshake timeout

The `/proto` WebSocket caps every inbound and outbound frame at
`MAX_PROTO_MESSAGE_SIZE` (1 MiB) - applied to both the inbound
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

- `tests::max_proto_message_size_is_sane` - regression for #110: cap stays
  at least `2 × NAR_PUSH_CHUNK_SIZE` (room for chunk + framing) and well
  below 16 MiB so a future refactor cannot silently relax the bound back
  toward tungstenite's 64 MiB default.
- `tests::handshake_timeout_is_sane` - regression for #110: deadline stays
  in `[5 s, 60 s]` so a real auth round-trip still fits but a stalled peer
  is dropped quickly.

## Worker - reconnect retries forever

`Worker<Disconnected>::reconnect` (`backend/worker/src/worker/mod.rs`) now
returns `Result<Worker<Connected>, (anyhow::Error, Self)>`: on failure, the
disconnected typestate (and the cached executor / scorer / credentials /
candidate maps) is handed back so the caller can retry without losing
state. The reconnect-with-backoff loop in `main.rs` is extracted to
`backend/worker/src/reconnect.rs::retry_reconnect` so it is unit-testable
without standing up a real `Worker`. The loop never gives up - a transient
network blip cannot terminate the worker process anymore (#99).

Tests (`cargo test -p worker --bins reconnect`):

- `reconnect::tests::keeps_retrying_after_failure` - regression for #99:
  the loop returns `Ok` only after several failed attempts, so a single
  transient error no longer breaks out and shuts the worker down.
- `reconnect::tests::backoff_caps_at_max` - delay sequence doubles from the
  initial backoff and plateaus at `max_backoff`.
- `reconnect::tests::state_threads_through_retries` - the same state value
  is threaded through every attempt, proving the typestate-preservation
  contract that the real `Worker<Disconnected>` relies on for cached
  resources.

## Typed DB pools - `WebDb` / `WorkerDb`

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
`web::endpoints::stats::get_cache_stats` - the cache-totals query was
reading from the worker pool while every other query in the same handler
used `web_db`; it now uses `web_db` consistently. The fire-and-forget
NAR-fetch bookkeeping in `web::endpoints::caches::nar` keeps using
`worker_db` on purpose (it should not contend with foreground HTTP
requests) and now carries a comment explaining the choice.

Tests (`cargo test -p core --lib types::db`):

- `types::db::tests::newtypes_are_non_substitutable` - regression for
  #68: a function typed `fn(&WebDb)` must not accept a `&WorkerDb` and
  vice versa, which is the compile-time defense the issue asked for.
- `types::db::tests::forwards_connection_trait` - `&WebDb` / `&WorkerDb`
  satisfy `&impl ConnectionTrait`, so existing SeaORM call sites keep
  working without `.inner()` boilerplate.

## Build status - `Created` collapsed to `Queued` for API responses

Issue #120: the frontend renders a coloured dot via
`status-{{ build.status.toLowerCase() }}`, but no `status-created` style
exists. `Created` is an internal-only transient state - the scheduler
flips builds to `Queued` almost immediately - so the API now collapses
it via `BuildStatus::for_api()` (`backend/entity/src/build.rs`) at every
response boundary (`evals::query`, `projects::evaluations`,
`projects::metrics`, `builds::query`).

Unit tests in `backend/entity/src/build.rs`:

- `for_api_collapses_created_to_queued` - `Created.for_api() == Queued`.
- `for_api_passes_through_other_states` - every other variant is
  returned unchanged.

## Shared web/core helpers (`#78`)

To collapse the boilerplate measured in issue #78, the following helpers
were introduced and applied repo-wide:

- `core::types::now()` - single source for `chrono::Utc::now().naive_utc()`,
  the timestamp shape every persisted column expects.
- `web::helpers::ok_json(message)` - wraps a value in the standard
  successful `BaseResponse` envelope, replacing the boilerplate
  `Json(BaseResponse { error: false, message })`.
- `web::helpers::OptionExt::or_not_found(resource)` - converts the
  result of a SeaORM `.one(db).await?` lookup into a `WebResult<T>`
  with a `<resource> not found` 404, replacing the
  `.ok_or_else(|| WebError::not_found(...))` chain.
- `WebError::{bad_request, unauthorized, forbidden, conflict,
  unprocessable_entity, internal, service_unavailable}` - accept
  `impl Into<String>` so callers can drop `.to_string()` on string
  literals and `format!(...)` payloads.
- `WebError::data_inconsistency(resource)` - for the recurring
  `"<resource> data inconsistency"` referential-integrity 500.

Unit tests in `backend/web/src/helpers.rs`:

- `ok_json_wraps_with_error_false` - the envelope is constructed with
  `error: false` and the supplied message.
- `or_not_found_returns_value_for_some` - passes the inner value through
  unchanged.
- `or_not_found_maps_none_to_not_found` - produces the expected
  `WebError::NotFound("Thing not found")`.

## Shared HTTP client (`#79`)

Eliminates the prior 18 ad-hoc `reqwest::Client::new()` /
`reqwest::Client::builder()` constructions across the workspace, which
each created a fresh TCP/TLS connection pool with inconsistent (or
absent) timeout and redirect policy.

`backend/core/src/http.rs` builds the project-wide client with sane
defaults (30 s timeout, `redirect::none`, and a branded
`Gradient/<version> (+https://github.com/wavelens/gradient)`
user-agent so upstream cache operators can attribute traffic). The
server stores it once on `ServerState::http`; the
worker exposes it through a `OnceLock` (`worker::http::client()`); the
CLI exposes it through `connector::http_client()`.

CI reporters (`GiteaReporter`, `GithubReporter`, `GithubAppReporter`)
and the GitHub-App helpers (`get_installation_token`, `exchange_code`)
now take the shared `reqwest::Client` as a parameter instead of building
their own.

Unit tests in `backend/core/src/http.rs`:

- `build_client_succeeds` - the default builder yields a usable
  `reqwest::Client`.
- `user_agent_includes_brand_and_contact_url` - the user-agent string
  starts with `Gradient/` and embeds the project URL so cache operators
  can identify and contact-trace outbound calls (`#205`).
- `user_agent_does_not_use_lowercase_brand` - regression guard against
  the previous lowercase `gradient/` format (`#205`).
- `init_crypto_provider_is_idempotent_and_enables_tls` - regression
  guard for `#232`: `init_crypto_provider` installs the rustls
  `aws-lc-rs` provider, may be called repeatedly, and unblocks
  `rustls::ClientConfig::builder()` (which otherwise panics when the
  process-level provider has not been chosen). Worker (`worker::main`)
  and server (`backend::main`) call it before any TLS handshake so
  `wss://` connections through `tokio_tungstenite::connect_async`
  succeed under TLS.

## Graceful shutdown (`#72`)

`backend/core/src/shutdown.rs` introduces a `Shutdown` primitive bundling a
`tokio_util::sync::CancellationToken` with a `tokio_util::task::TaskTracker`.
It replaces bare `tokio::spawn` for every long-lived background task -
dispatch loops, the outbound worker connection loop, the cache GC and
sign-sweep loops, webhook deliveries, CI reporters, and the fire-and-forget
metric writes from the NAR cache surface. `serve_web` installs a
SIGINT/SIGTERM handler that calls `shutdown.cancel()`, hands the token to
`axum::serve(...).with_graceful_shutdown(...)`, then awaits
`shutdown.cancel_and_drain(30s)` so in-flight cleanups, metric writes, and
webhook deliveries finish before the process exits.

Unit tests in `backend/core/src/shutdown.rs`:

- `cancel_interrupts_select_loop` - a task that `select!`s on
  `cancelled()` against a 60-second sleep returns immediately when the
  token fires.
- `drain_waits_for_in_flight_work` - `cancel_and_drain` waits for
  spawned futures to finish (no abandonment of in-flight work).
- `drain_timeout_returns_false` - a task that ignores the cancel
  signal is reported as a drain timeout, not silently abandoned.
- `child_token_cascades_from_parent` - child tokens used for
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

- `no_dependents_returns_only_start` - a leaf derivation yields a set
  containing exactly the starting id.
- `walks_multiple_layers_breadth_first` - a 3-layer graph is fully
  visited, including a sibling that depends directly on the start.
- `cycles_terminate` - a pathological reverse cycle is deduped via the
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
evaluation. `Aborted` is never propagated - when a leader is aborted (its
own evaluation cancelled) `abort_evaluation` re-elects a new leader from
the surviving followers instead of dragging unrelated evaluations down.

`find_active_leaders` (now in `core::db::status`) is the single source of
truth for the leader lookup, called both from `eval::insert_build_rows`
(regular eval-result path) and `ci::trigger::trigger_restart_builds`
(rerun-failed-builds path) so the two paths can't diverge.

Tests:

- `dispatch_tests::dispatch_skips_follower_builds` - the SQL gate keeps
  followers out of the dispatcher result set, so no follower job is ever
  enqueued.
- The full pre-existing `handle_build_job_completed` /
  `handle_build_job_failed` mock-DB suite was extended to mock the
  `propagate_to_followers` followers query, exercising the new code path
  on every terminal transition.
- `ci::trigger::tests::restart_sets_via_when_leader_active_elsewhere` -
  when the "rerun failed builds" path finds an in-flight leader for a
  derivation it's about to re-queue, the new build row carries
  `via = leader.id`. Verified by draining the MockDatabase transaction
  log and asserting the leader's UUID appears in the INSERT's value
  list.

## Typed entity IDs (`entity::ids`)

`backend/entity/src/ids.rs` defines one newtype per entity (`UserId`,
`OrganizationId`, `ProjectId`, …) so the compiler rejects argument
swaps. Unit tests (`cargo test -p entity --tests`) cover:

- Round-trip with `Uuid` (no information loss).
- `serde` transparency (wire format identical to bare `Uuid`).
- `FromStr` parsing (lets axum `Path<UserId>` extract from URL segments).
- `TryFromU64` returns `DbErr` (UUID PKs are never `u64`-derivable).
- `Default` resolves to `Uuid::nil()` so `Id::default() == Id::nil()` —
  enables `Model { id, ..Default::default() }` ergonomics.

## Model defaults (`entity::model_default_tests`)

Every `DeriveEntityModel` struct derives `Default`, and every
`DeriveActiveEnum` column type has a `#[default]` variant (initial-state /
fail-noisy where applicable). Smoke tests in `backend/entity/src/lib.rs`
confirm the derive resolves for representative models:

- `user::Model::default()` — strings empty, `id` is nil, no password.
- `build::Model::default()` — `status == BuildStatus::Created`.
- `evaluation::Model::default()` — `status == EvaluationStatus::Queued`.
- `audit_log::Model::default()` — JSON metadata is `None`, timestamp is
  the 1970 epoch from `NaiveDateTime::default()`.
- `organization_cache::Model::default()` — `mode == ReadWrite`.

A nil ID is a placeholder, not a persistable value: callers override `id`
(typically with `Id::now_v7()`) and use `..Default::default()` for the
remaining fields.

A `trybuild` compile-fail test
(`cargo test -p entity --test compile_fail`) locks the swap-prevention
property: a function expecting `OrganizationId` MUST reject a `UserId`
argument at compile time. Regenerate the captured rustc diagnostic
after a deliberate API change with:

    TRYBUILD=overwrite cargo test -p entity --test compile_fail

## NAR streaming - bounded backend reads

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

## Proto writer - peer-stall detection

`proto/src/handler/socket.rs::writer_tests`:

- `send_msg_times_out_when_queue_is_full` constructs a `ProtoWriter`
  whose drain task is intentionally absent, fills the bounded queue, and
  asserts the next `send_msg` returns `Err(())` after
  `send_chunk_timeout` instead of blocking forever. This is the
  producer-observable signal that a peer's TCP receive side has stalled
  - the failure unblocks the dispatch loop instead of letting the
  worker's 600 s receive ceiling fire.
- `send_msg_succeeds_when_queue_has_room` covers the fast path: a
  serialised message lands in the channel without delay when there's
  capacity.

## Proto NAR serving - streaming, chunking, and missing paths

`proto/src/handler/socket.rs::serve_nar_tests`:

- `serve_streams_full_payload_in_chunks` puts a 9 MiB NAR into a local
  `nar_storage`, calls `serve_nar_request`, and asserts the spy writer
  observed ≥ 3 `NarPush` frames whose concatenated `data` equals the
  source. The last frame must have `is_final = true`. Locks the
  invariant that streaming serving preserves wire semantics.
- `serve_emits_nar_unavailable_when_missing` confirms a missing hash
  surfaces as exactly one `NarUnavailable` frame plus an `Err` return -
  no `NarAbort`, no orphan `NarPush`.

## Per-session NAR upload buffer - bounded memory (issue #109)

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
  budget, not a per-path one - many small open uploads cannot collude
  to exceed the limit.

## Auth hardening - sessions, API key lifecycle, account deletion (issue #91)

`backend/web/tests/auth_hardening.rs` drives the production router with a
`MockDatabase` and signs synthetic JWTs against the same secret the test
state holds. Each test pins one revocation/expiry rule to a specific HTTP
status so a regression cannot quietly weaken the surface:

- `jwt_with_revoked_session_is_rejected` and `jwt_with_expired_session_is_rejected`
  prove that a JWT alone is no longer sufficient - the auth middleware
  loads the matching `session` row and refuses anything revoked or past
  `expires_at`. This is what makes logout effective (issue #104).
- `jwt_with_unknown_session_is_rejected` covers the case where the row was
  deleted: the token must fail closed.
- `revoked_api_key_is_rejected` and `expired_api_key_is_rejected` lock in
  the same checks for `GRAD…` keys (issue #44). A revoked or expired key
  returns 401 even if the hash still matches.
- `delete_user_without_password_is_forbidden` and
  `delete_user_with_wrong_password_is_forbidden` enforce the re-auth
  requirement on `DELETE /user` - a stolen JWT cannot wipe a
  password-auth account on its own (issue #43).

Run with `cargo test -p web --test auth_hardening`.

## Evaluation `waiting_reason` - surfaces the reconciler verdict (issue #98)

`backend/scheduler/src/build.rs::waiting_reason_tests` exercises
`BuildabilityChecker::compute_waiting_reason` directly so the API payload
returned by `GET /evals/{evaluation}` is locked in:

- `no_workers_lists_every_unique_arch` - when no worker is connected, every
  pending build's `(architecture, required_features)` combo lands in
  `unmet`, with `connected_workers == 0`.
- `satisfied_builds_are_excluded_from_unmet` - pending builds whose arch
  matches some connected worker are filtered out; only the genuinely
  blocked combos remain.
- `missing_feature_is_reported_alongside_arch` - a build whose arch is
  available but whose `requiredSystemFeatures` aren't satisfied is
  reported with the missing feature names attached.
- `identical_requirements_are_grouped_with_count` - N pending builds with
  the same blocking requirement collapse to one `UnmetRequirement` with
  `build_count == N`, so the UI doesn't repeat itself.
- `builtin_arch_satisfied_by_any_worker` - `architecture == "builtin"`
  derivations are never counted as unmet so long as any worker is
  connected.
- `pre_build_target_queued_no_workers_stalls_to_waiting` - when an
  evaluation is in `Queued` and no worker is connected, the reconciler
  picks `Waiting` with an empty `unmet` list so the UI can explain the
  stall without inventing fake build requirements (issue #97).
- `pre_build_target_waiting_with_workers_recovers_to_queued` - once any
  worker is connected, a `Waiting` eval is recovered to `Queued` so the
  dispatch loop replays the normal progression.
- `pre_build_target_waiting_no_workers_keeps_waiting` - a `Waiting` eval
  with no workers stays in `Waiting` but its `WaitingReason` is
  refreshed.
- `pre_build_target_queued_with_workers_is_noop` - a `Queued` eval with
  workers connected needs no reconciliation; the dispatcher will pick it
  up.
- `pre_build_target_active_pre_build_with_workers_left_alone` - a
  `Fetching` / `EvaluatingFlake` / `EvaluatingDerivation` eval is owned
  by an eval worker. The reconciler must not push it back to `Queued`,
  which the state machine forbids and would produce a spurious
  "invalid status transition: Fetching → Queued" warning.
- `pre_build_target_active_pre_build_no_workers_stalls` - the same
  active pre-build states do stall into `Waiting` when every worker has
  disconnected.

Run with `cargo test -p scheduler --tests waiting_reason_tests`.

## Pre-build evaluation stall when no worker exists (issue #97)

`backend/core/src/state_machine/eval.rs::tests` extends the evaluation
state machine to allow the scheduler to surface a "no worker connected"
stall before any builds have been queued:

- `eval_sm_pre_build_states_can_enter_waiting` - every pre-build status
  (`Queued`, `Fetching`, `EvaluatingFlake`, `EvaluatingDerivation`) can
  transition to `Waiting`, matching what `BuildStateHandler::reconcile_waiting_state`
  now does when `worker_caps.is_empty()`.
- `eval_sm_waiting_recovers_to_queued` - the recovery edge `Waiting →
  Queued` is valid; this is the path the reconciler takes once a worker
  reconnects, so the dispatch loop replays the normal pre-build chain
  rather than skipping straight back into a later phase.
- `eval_sm_waiting_cannot_skip_into_pre_build_phases` - direct
  `Waiting → Fetching/EvaluatingFlake/EvaluatingDerivation` transitions
  are rejected so that recovery always flows through `Queued`.

Run with `cargo test -p core --tests state_machine::eval`.

## Project triggers (issue #116)

- `core::types::triggers` - round-trip serialisation, polling interval validation (≥10s), polling branch field (optional, nullable), six-field cron parsing, type/JSON shape mismatches.
- `core::ci::abort` - `abort_evaluation` hard vs soft, terminal eval no-op.
- `core::ci::apply` - `apply_trigger` orchestration: same-commit dedup, time-trigger and manual bypass, project-level concurrency policies (skip / hard_abort / soft_abort / all). The `all` policy creates a new evaluation alongside a running one; the new row carries `concurrent = true`.
- `core::state::provisioning` - trigger config builder helpers, integration name resolution, key stability.
- `scheduler::trigger_dispatch` - `polling_due` and `cron_due` boundary conditions; `dispatch_once` no-trigger and within-interval skip cases; polling jitter bounded to 10% of `interval_secs`, deterministic for a `(trigger_id, last_fired_at)` pair, varies across cycles, zero when `interval_secs < 10`, and gates firing exactly at the `interval + jitter` boundary.
- `scheduler::jobs::JobTracker::remove_job` - pending and active map removal; unknown id no-op.
- `scheduler::Scheduler::cancel_evaluation_jobs` - drops eval and per-build entries from the tracker.
- `web::endpoints::projects::triggers` - list/create/read/update/delete; `all` concurrency accepted (200); invalid config rejected (400).
- `web::endpoints::projects::triggers` - **integration enrichment**: list/get responses for `reporter_push`/`reporter_pull_request` include an inlined `integration` object (`id`, `name`, `display_name`, `forge_type`); polling triggers return `integration: null` and skip the integration SELECT (`list_polling_trigger_has_null_integration_and_skips_lookup`); orphaned references (integration row deleted) degrade to `integration: null` (`list_reporter_trigger_with_missing_integration_returns_null`).
- `web::endpoints::orgs::integrations` - **summaries endpoint** (`GET /orgs/{org}/integrations/summary`): any org member can list summaries (no `ManageIntegrations` required); response excludes `secret`, `endpoint_url`, `access_token`, `has_secret`, `has_access_token` so non-admins cannot probe credential state; non-members get 404 (consistent with org loader's hide-existence policy).
- `web::endpoints::projects::evaluations` - response includes nullable `trigger` summary, populated for evaluations created by a trigger.
- `web::endpoints::forge_hooks::events` - PR (github/gitea/gitlab) and release (github/gitea/gitlab) parsers; GitLab action mapping; tag-ref support on push parsers.
- `web::endpoints::forge_hooks` integration - push fans out to matching trigger row; branch glob filter skip; PR action filter; release fires only `releases_only` triggers; GitHub App push by installation_id.
- `web::endpoints::projects::management` - creating a project seeds a default polling trigger.

## Proto wire decoders - alignment-safe deserialisation

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
  and `…::decode_server_message_handles_misaligned_input` -
  encode a representative message, place the bytes at a deliberately
  misaligned address (`AlignedVec<16>` base + 1) so the input pointer is
  guaranteed not to be 16-byte-aligned, then assert the helper still
  decodes back to the original value. This is the regression for the
  reconnect-time deserialisation failures observed when the server's
  inbound buffer happened to land at a non-16-byte-aligned allocator
  address.

## NAR upload integrity - buffer-overflow poisoning, abort propagation, self-heal

Four interlocking bugs let a session-buffer overflow produce a build that the
server marked `Completed` while the path's NAR was never persisted:

- `proto/src/handler/dispatch.rs` (`NarBuffers::append`) returned an error on
  overflow but **did not poison the path** - subsequent chunks for the same
  path could land in the buffer if the budget freed up, the partial buffer was
  retained, and `on_nar_uploaded` would still call `mark_nar_stored` because
  `nar_buffers.take()` returned the (now bogus) bytes or `None` (treated as
  S3 mode).
- `worker/src/executor/mod.rs` `execute_build_job` did not receive the
  per-job `abort_rx` watch, so a server-side `AbortJob` had no path through
  the build / compress / push loop. The worker silently kept streaming and
  ended with `JobCompleted`.
- `worker/src/proto/job.rs` `request_nars` registered each path's waiter
  inside its `await` rather than synchronously before sending `NarRequest`,
  so any server response that raced ahead found no waiter and surfaced as a
  *"received NarUnavailable/NarAbort with no waiter - discarding"* warning.
- `proto/src/handler/socket.rs` `serve_nar_request` left lying `cached_path`
  rows behind when `nar_storage.get_stream(hash)` reported the NAR missing,
  so the next worker requested the same missing path forever.

Tests:

- `proto::handler::dispatch::nar_buffers_tests::append_overflow_drops_partial_buffer_and_poisons_path`
  (`cargo test -p proto`) - first overflowing chunk drops the partial buffer,
  releases bytes back to the budget, marks the path poisoned, and any further
  chunks return `AppendOutcome::Poisoned`.
- `proto::handler::dispatch::nar_buffers_tests::clear_poison_allows_retry`
  - a retry on a fresh job/path key clears the flag.
- `proto::nar_recv::tests::register_synchronously_installs_waiter_before_response`
  (`cargo test -p worker`) - every path in a batched `NarRequest` has a
  live waiter at the time the server's first response arrives, including
  paths whose siblings already failed.
- `executor::compress::tests::check_abort_returns_err_after_signal`
  (`cargo test -p worker`) - once the dispatch loop signals `abort_tx`,
  `compress_and_push_paths` propagates the abort as an `Err`, which becomes
  a `ClientMessage::JobFailed` instead of `JobCompleted`.

`serve_nar_request`'s self-heal demote (`invalidate_cached_path`) is
exercised end-to-end whenever an integration test routes through the
`NarRequest` path with a missing NAR - the row is updated to
`file_hash = NULL` so `Model::is_fully_cached()` returns `false` and the
next `CacheQuery` no longer reports the path as cached. We deliberately
demote rather than delete: `derivation_output.cached_path` is `ON DELETE
SET NULL`, so a delete would silently drop the link plus the
`cached_path_signature` placeholders, while a demote keeps the row's
identity and lets a subsequent successful upload re-fill the metadata.

## Worker aborts the running build, not just the upload (#309)

Abort propagation reached the `compress`/`push` loop (above), but **not the
derivation build itself**. `execute_build_job` only checked `abort` at the top
of each task iteration; the long-running `build_derivation` never observed it.
So cancelling a build server-side left the worker's nix-daemon compiling the
derivation to completion - high memory, build still running, `LogChunk`s still
streaming - while the server already showed no running build.

Fix in `worker/src/executor/build.rs`: the build log drain races the daemon's
log stream against the `abort` watch (`next_log_event`). When the server
signals `AbortJob`, `drain_build_logs_with_timeout` returns
`DrainOutcome::Aborted` and `realize` returns early *without* `mark_ok`.
Dropping the `ScopedGuard` discards the daemon connection (closes the socket),
which makes the nix-daemon kill the in-flight build. Because the build errors
out before the compress/push stage, no NAR is uploaded for a cancelled build.

Tests (`cargo test -p worker executor::build::tests`):

- `next_log_event_returns_aborted_when_already_set` - abort already signalled
  before the first poll yields `NextLog::Aborted` immediately, even against a
  stalled stream.
- `next_log_event_aborts_while_waiting_on_stalled_stream` - a stream that never
  yields stays `Pending` until `abort` fires, then resolves to
  `NextLog::Aborted` (proves a running build is interruptible, not hung).
- `next_log_event_reports_stream_end` - an exhausted log stream maps to
  `NextLog::StreamEnd` so a normal build still completes.
- `next_log_event_errors_on_silent_timeout` - with `maxSilent` set and no log
  output, the drain errors once the budget elapses (paused-time test).

## Startup recovery preserves queued/waiting work

`core/src/db/connection.rs::update_db` runs once on boot to reconcile work
left behind by the previous process. It used to abort **everything** active -
every `Created`/`Queued`/`Building` build and every `ACTIVE` evaluation - which
needlessly threw away queued evaluations and builds that were only waiting for
a free worker.

The policy is now two pure predicates the recovery loop applies per row:

- `eval_survives_restart` - `Queued` and `Waiting` evaluations survive (the
  eval dispatcher re-offers `Queued`; build reconcile re-drives `Waiting`).
  `Fetching` / `EvaluatingFlake` / `EvaluatingDerivation` / `Building` were
  running on a now-disconnected worker, so they are aborted (and their project
  is flagged `force_evaluation`).
- `build_survives_restart` - a build survives only if it is `Created`/`Queued`
  **and** its evaluation survives. A `Building` build was mid-compile on a lost
  worker; a queued build under an aborted evaluation goes with it.

Tests (`cargo test -p core startup_recovery_tests`):

- `queued_and_waiting_evaluations_survive_restart`
- `actively_running_evaluations_are_aborted_on_restart`
- `queued_builds_of_a_surviving_evaluation_survive_restart` - the "eval waiting
  case" from the bug report: builds queued for a free worker are kept.
- `running_builds_are_aborted_even_under_a_surviving_evaluation`
- `builds_of_an_aborted_evaluation_are_aborted`

## Cache GC - guard shared-hash NARs and purge zombie cached_path rows

Two bugs together inflated cache stats and over-deleted shared NARs:

- `gc_orphan_derivations` deleted the NAR for every output of every orphan
  derivation, with no check whether another (non-orphan) `derivation_output`
  shared the same hash via `cached_path`. FOD source tarballs are the
  textbook case - `fetchurl` derivations across many projects all
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

- `purges_cached_paths_whose_nar_is_missing` - feeds a `cached_path`
  whose hash is absent from the local NAR store and asserts the live
  NAR is preserved while the orphan-files pass exercises the new
  cleanup branch.

## Worker recovers build outputs when the daemon drops them on the wire

`harmonia_protocol::BuildResult` deserializes `built_outputs` only when
the negotiated protocol advertises the `realisation-with-path-not-hash`
feature; on a daemon old enough to predate that feature, harmonia drains
the legacy `StringMap` form and returns an empty `BTreeMap`. The
worker's `ParsedDerivation::realize`
(`backend/worker/src/executor/build.rs`) consumed `s.built_outputs`
directly, so a successful local build against such a daemon produced
`Vec<BuildOutput>::new()` - the worker reported
`BuildOutput { outputs: [] }`, the server's `handle_build_output` had
nothing to iterate, no `derivation_output` was updated with `nar_size`,
no `build_product` rows were written, and the `/builds/{id}/downloads`
endpoint came back empty even though `nix-support/hydra-build-products`
existed on disk under the realised output path.

Recovery now happens in
`output_pairs_from_built_or_drv`: when `built_outputs` is empty, fall
back to the parsed `.drv`'s declared outputs (input-addressed drvs
already carry the exact paths nix will produce). Outputs whose `.drv`
entry has an empty `path` (content-addressed / deferred) are skipped -
those genuinely require the daemon's response. The fallback emits a
`warn!` so an old-daemon environment is visible in the worker log
instead of failing silently.

Tests (`cargo test -p worker --tests output_pairs`):

- `output_pairs_use_built_outputs_when_daemon_returned_them` - modern
  protocol path: when `built_outputs` is non-empty, the helper passes
  it through and ignores the `.drv` (whose path may be stale for
  CA-derivations).
- `output_pairs_fall_back_to_drv_when_daemon_dropped_built_outputs` -
  regression: empty `built_outputs` plus a multi-output `.drv` yields
  one pair per declared output. Locks in the recovery path so a future
  refactor doesn't re-introduce the silent empty-report.
- `output_pairs_skip_drv_outputs_with_empty_path` - CA / deferred
  outputs in the `.drv` (empty `path` field) are dropped from the
  fallback rather than producing malformed `/nix/store/` strings.

## Pull-mode CacheQuery surfaces unsatisfiable paths explicitly

The `query` handler in `backend/proto/src/handler/cache.rs` previously
omitted any path it could not satisfy in `Pull` mode: a path that was
neither in the local `cached_path` table nor returned by the upstream
narinfo probe simply did not appear in the `CacheStatus` response. The
worker's prefetch closure walk could not distinguish "the server has
nothing for this path" from "the server was never asked", so its
hard-fail guard (`InputPrefetcher::query_and_split` in
`backend/worker/src/proto/nar_import.rs`) never fired. The path was
silently skipped, and the build only failed several layers later when
the local nix-daemon rejected an `add_to_store_nar` for a *dependent*
path with a confusing `path '…' is not valid` error referencing the
silently-dropped reference.

Pull mode now mirrors Push mode: every queried path appears in the
response, with `cached: false` (and no metadata) for paths the server
cannot serve. The worker's existing `classify_cached_entries` already
handles the `Uncached` variant, so the closure walk now hard-fails with
the intended `… missing from local store and not available in the
gradient cache` message before any import is attempted.

Tests (`cargo test -p proto --tests cache_query`):

- `cache_query_pull_uncached_returns_entries_with_cached_false`
  - replaces the previous `cache_query_pull_uncached_returns_empty`.
  With an empty mock DB the handler must return one `cached: false`
  entry per queried path, all metadata fields `None`. Locks the new
  Pull-mode contract so a future regression to the silent-omit
  behaviour fails the test instead of surfacing as a daemon error
  during a real build.
- `cache_query_normal_uncached_returns_empty` - preserved unchanged.
  Normal mode is consumed by `mark_substituted` in
  `backend/worker/src/executor/eval.rs`, which iterates returned
  entries to flip a `substituted` flag without inspecting `cached`;
  emitting `cached: false` entries there would mislabel uncached
  derivations as substituted. The asymmetry between Pull and Normal is
  intentional and the test makes it explicit.

## Substituted classification - match cached_path by hash, not by foreign-key link

`compute_truly_substituted` previously demanded
`derivation_output.cached_path IS NOT NULL` and `is_cached = true` to mark
a drv as `Substituted`. That link is set lazily by `mark_nar_stored` on
upload, so a re-evaluated drv whose output hash was already in
`cached_path` (shared FOD source, manual cache push, fresh eval before
its first upload) was misclassified as needing a build and rerun every
time. The worker's `CacheQuery` handler already merges by hash for the
same reason; the eval-time decision now does too.

Tests (`cargo test -p scheduler --tests substitut`):

- `eval_result_substituted_derivation_completes_eval` - original happy
  path: linked cached_path with file_hash → drv marked Substituted, eval
  completes immediately.
- `eval_result_substitutes_when_hash_in_cached_path_without_link` -
  regression: derivation_output with `is_cached = false` and
  `cached_path = None`, but a `cached_path` row with the same hash and
  `file_hash IS NOT NULL` exists. The drv is marked Substituted and the
  eval completes without dispatching a build. Confirms the hash-based
  fallback in `compute_truly_substituted`.

## Substituted at build time - outputs already valid on the worker (#303)

When the daemon reports a build's outputs as already valid (empty
`built_outputs`), no build actually ran. The worker now sets the
`substituted` flag on its `BuildOutput` job update; `handle_build_output`
moves the build to `Substituted`, and `handle_build_job_completed`
preserves that terminal status instead of overwriting it with `Completed`.
This is distinct from eval-time `compute_truly_substituted` (above), which
covers outputs already cached before evaluation.

Tests:

- `build_sm_building_to_substituted` (`core`) - the `Building → Substituted`
  transition is permitted by the state machine.
- `build_completed_preserves_substituted_status` (`scheduler`) - a build
  already moved to `Substituted` keeps that status on `JobCompleted`; the
  `from == to` transition skips the build UPDATE and the evaluation still
  finalises as `Completed`.

## Scheduler policy - anti-starvation cap (#112)

`WaitTimeRule::max_wait_secs` caps how much wait time can contribute to a
job's score. The previous default (600s, +60 max) was below
`MissingPathsRule::scored_bonus` (200), so a steady stream of fresh
fully-cached candidates outscored older queued builds indefinitely -
builds older than 10 minutes were no longer differentiated by wait time.
The default is now 3600s (+360 max), enough to overcome the cached-fresh
preference plus typical penalties on the older job. The scoring rules and
the composed default policy now live in the `score` crate.

Tests (`cargo test -p score policy`):

- `simple_policy_long_waiting_build_overcomes_fresh_cached` -
  (`score::policy`) locks in the anti-starvation guarantee by composing
  the full simple policy via `policy_by_name("simple")`: a build queued
  an hour ago must outscore a fresh candidate the worker can serve
  directly. Fails if `WaitTimeRule::max_wait_secs` is lowered back below
  the `MissingPathsRule` scored bonus.
- `simple_policy_prefers_ready_over_costly` - (`score::policy`) a ready
  job (0 missing paths, 0 NAR, real arch) must outscore a builtin job
  with 5 missing paths and a 50MB NAR.
- `wait_time_longer_wait_scores_higher_but_capped` -
  (`score::rules::builtin`) per-rule guard: asserts the score saturates
  at `max_wait_secs * bonus_per_second` so ancient jobs cannot dominate
  every other rule.

## Scheduler policy - per-org fair share (#111)

A tenant flooding the queue with hundreds of builds previously starved a
quiet tenant: `WaitTimeRule` plateaus at +360, so once the busy org's jobs
maxed out their wait bonus there was nothing left to favour the quiet org.
`FairShareRule` penalizes a candidate proportional to its owning org's
share of currently-active builds (`org_share`, computed in-memory by the
scheduler in `take_best_of_kind` from the active-job map and threaded into
`JobContext`). The default weight (500) exceeds the wait plateau (360) so
fairness overrides the wait gradient. The rule is part of the
`resource-aware` policy; `org_share` is `None` (no penalty) when no builds
are active.

Tests (`cargo test -p score fair_share`, `cargo test -p scheduler fair_share`):

- `busier_org_scores_more_negative` - (`score::rules::fair_share`) a job
  with `org_share = Some(0.99)` scores more negative than `Some(0.01)`.
- `zero_share_and_none_score_zero` - (`score::rules::fair_share`)
  `Some(0.0)` and `None` both contribute exactly 0.
- `fair_share_overrides_wait_gradient` - (`score::rules::fair_share`) a
  quiet org (share 0) outscores a busy org (share 1) even when the busy
  job has saturated `WaitTimeRule`, proving the weight beats the plateau.
- `fair_share_quiet_org_wins_over_busy_org` - (`scheduler::jobs`) end to
  end: org A already has five active builds, org B none; with the
  `resource-aware` policy the next build is assigned to B's pending job.

## Build artefacts - `external_cached` outputs include `hydra-build-products`

Builds that are dispatched as `external_cached` (substituted from upstream,
not rebuilt locally) used to report `products: Vec::new()` even when the
fetched output contained `nix-support/hydra-build-products`, leaving the
artefacts page empty for any drv that was already on `cache.nixos.org`.
The worker's external-cache branch now calls `load_products` on each
fetched output path, the same loader the regular build path uses.

Tests (`cargo test -p worker executor::build::tests`):

- `load_products_returns_empty_when_file_absent` - the loader is a no-op
  when the output has no `nix-support/hydra-build-products`, so substituted
  outputs without artefacts remain artefact-free.
- `load_products_parses_hydra_lines` - a `file html …/index.html` line in
  `nix-support/hydra-build-products` produces one `BuildProduct` with the
  `file_type`, `subtype`, `name` (basename), and `size` (stat) populated.
  Regression for substituted/external-cached builds whose artefacts never
  reached the `build_product` table.

## CI pending status fires at queue time (#117, superseded by #262)

Per-event CI status reporting from the scheduler/web layer has been
removed in favour of `ForgeStatusReport` actions (issue #262). Deployments
that want a `Pending` check on the commit at queue time must configure a
`ForgeStatusReport` action subscribed to `evaluation.queued`; the
scheduler no longer spawns reports unconditionally.

## Enum primitive conversions via `num_enum` (#80)

`BuildStatus`, `EvaluationStatus`, `IntegrationKind`, `ForgeType`,
`TriggerType`, and `ConcurrencyPolicy` derive
`num_enum::IntoPrimitive`/`TryFromPrimitive` instead of hand-rolled
`as_i16`/`from_i16`/`num_value` helpers. Database rows still use the
explicit discriminants - moving them in source would silently break the
on-disk encoding.

The `concurrency_round_trip` and `trigger_type_round_trip` tests in
`core/src/types/triggers.rs` cover the integer ↔ enum mapping and assert
that out-of-range values produce an error rather than panicking.

## `GET /commits/{commit}` authorization (#88)

The endpoint historically returned commit metadata to any authenticated
caller - the handler held a `// TODO: Check if user has access to the
commit` and never enforced it, allowing cross-tenant disclosure of
commit message, hash, and author for any commit UUID an attacker could
guess or harvest. The route now lives behind `authorize_optional` and
the handler walks `commit → evaluation → project → organization` to
require either public visibility or membership; every other case
(non-member, anonymous on private org, missing commit, no referencing
evaluation) maps to `404` so existence isn't leaked.

Tests (`cargo test -p web --test commits_authorization`):

- `anon_can_read_commit_in_public_org` - an unauthenticated caller may
  fetch a commit reachable through a project in a public organization.
- `anon_cannot_read_commit_in_private_org` - the same commit, but the
  organization is private, returns `404` for an unauthenticated caller.
- `member_can_read_commit_in_private_org` - an authenticated member of
  the owning organization sees the commit (200).
- `non_member_cannot_read_commit` - an authenticated user who is not a
  member of any organization that owns a referencing evaluation gets
  `404`. Direct regression for #88.
- `commit_referenced_only_via_orphan_eval_returns_404` - when every
  referencing evaluation has no `project` (legacy direct-build rows
  before issue #234), no org can be resolved and the response is `404`.
- `nonexistent_commit_returns_404` and
  `commit_without_evaluation_returns_404` - both shapes of "no path"
  return `404` without leaking which case applied.

## Proto WebSocket connection cap (#89)

`max_proto_connections` (env `GRADIENT_MAX_PROTO_CONNECTIONS`, default
256) was previously declared as configuration but never read - workers
could open `/proto` WebSockets without bound, exhausting file
descriptors, scheduler slots, and memory. The proto upgrade handler now
holds a permit on a `ProtoLimiter` (a `tokio::sync::Semaphore` sized
from the config) for the lifetime of each connection; once the limit is
hit, further upgrade attempts get `503 Service Unavailable` with
`Retry-After: 10`.

Unit tests (`cargo test -p proto handler::limiter`):

- `new_clamps_zero_capacity_to_one` - a misconfigured `0` collapses to
  `1` so the endpoint never silently rejects every upgrade; operators
  who want the endpoint disabled set `discoverable = false`.
- `try_acquire_returns_none_when_exhausted` - at capacity the next
  acquire fails immediately rather than queueing.
- `dropping_permit_releases_slot` - the slot is reclaimed when the
  permit is dropped, which corresponds to the upgraded session ending.
- `in_use_tracks_held_permits` - the operator-visible `in_use()` count
  matches the number of live permits (used in the rejection log line).

Integration tests (`cargo test -p web --test proto_connection_limit`)
cover the wiring of the limiter into the proto router:

- `upgrade_rejected_with_503_and_retry_after_when_limit_exhausted` - a
  WS-shaped GET against a saturated limiter returns `503` with the
  documented `Retry-After: 10` header. Direct regression for #89.
- `upgrade_proceeds_past_limiter_when_slot_is_free` - a fresh limiter
  does not produce the rejection response, confirming the handler only
  short-circuits on exhaustion.
- `slot_is_released_for_subsequent_upgrades_after_drop` - a held permit
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

- `from_db_err_passes_through_non_db_errors` - non-`DbErr::Query`
  variants (e.g. `RecordNotFound`) round-trip as `WebError::Internal`
  rather than being misclassified as conflicts.
- `from_db_err_passes_through_query_string_errors` - a `DbErr::Query`
  carrying a string-only payload (no underlying `sqlx::Error`) is
  treated as `Internal`.
- `from_db_err_record_not_found_is_internal` - pins the documented
  behaviour that "row missing" is the caller's pre-check problem, not
  a 409.

The mapper uses the typed sqlx 0.8 API (`db_err.is_unique_violation()`)
rather than scraping `to_string()`, so it survives sqlx upgrades that
reflow the message text.

Unit tests for the `TempUploadDir` RAII guard used by the direct-build
upload path live in `backend/web/src/endpoints/builds/direct.rs`:

- `temp_upload_dir_drop_removes_directory` - dropping the guard
  without calling `commit()` removes the on-disk staging directory, so
  a failed DB transaction cannot leave orphaned NARs behind.
- `temp_upload_dir_commit_keeps_directory` - `commit()` consumes the
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

- `missing_request_id_is_generated` - a request without `x-request-id`
  comes back with one, and the value parses as a UUID. Confirms the
  `MakeRequestUuid` minter is wired in front of `TraceLayer`.
- `supplied_request_id_is_echoed` - a request that *does* carry
  `x-request-id` has its value preserved verbatim on the response, so a
  reverse-proxy that injects an upstream trace id keeps the trace
  stitched end-to-end.
- `each_request_gets_a_distinct_id` - successive auto-generated ids
  differ; otherwise log correlation collapses across concurrent
  requests on the same connection.

## FK-chasing data-inconsistency log level (#85)

Access-context loaders (`EvalAccessContext` in
`backend/web/src/endpoints/evals/mod.rs`, `BuildAccessContext` in
`backend/web/src/endpoints/builds/mod.rs`, the derivation lookup in
`backend/web/src/endpoints/builds/query.rs`) chase from a child row to
its parent through FK columns. When the parent is missing - almost
always a transient race against a concurrent delete - the previous
implementation logged the event twice at error level: once at the
callsite and again inside `WebError::IntoResponse` for the wrapping
`Internal` variant. That noise drowned legitimate server errors.

The fix introduces a dedicated `WebError::DataInconsistency` variant.
External behaviour is unchanged (HTTP 500, code `internal`, body
`Internal server error`); the difference is operational:

- `IntoResponse` no longer logs for the new variant - the rich-context
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

## Worker eval-pool graceful shutdown (#95)

`EvalWorkerPool` now exposes `shutdown()`, which closes the acquire
semaphore, flips a `shutting_down` flag observed by
`PooledEvalWorker::drop`, and concurrently sends each idle worker the
`EvalRequest::Shutdown` opcode (waiting up to 5 s per child for a clean
exit). The worker binary installs a SIGINT/SIGTERM handler that cancels
a `tokio_util::sync::CancellationToken`; the dispatch loop, the
listener, and the reconnect/back-off loop all observe the token and
break out, after which `JobExecutor::shutdown` drains the pool. This
replaces the previous SIGKILL-on-drop path, which leaked GC roots and
temp files because libnix's atexit handlers never ran.

Unit tests in `backend/worker/src/worker_pool/pool.rs`:

- `shutdown_with_no_idle_workers_returns_immediately` - `shutdown()` on
  a fresh pool must not block.
- `shutdown_drains_idle_workers_gracefully` - pre-populated idle workers
  are sent through the graceful path and the idle vec is empty
  afterwards. Uses a `cat` subprocess as a stand-in worker so the test
  does not need libnix.
- `acquire_after_shutdown_errors` - once `shutdown()` returns, further
  `acquire()` calls fail fast with a "semaphore closed" error.
- `inflight_worker_shuts_down_gracefully_on_pool_shutdown` - when
  `shutdown()` runs concurrently with an in-flight `PooledEvalWorker`
  being released, the released worker takes the graceful-shutdown
  branch in `Drop` instead of being pushed back into the (now drained)
  idle vec.

## Prometheus metrics endpoint - `web/tests/metrics.rs`

Covers `GET /metrics`, the Prometheus exposition endpoint introduced
in #35:

- `endpoint_404_when_no_token_configured` - when
  `GRADIENT_METRICS_TOKEN_FILE` is unset, the route is not mounted and
  the global 404 fallback handles the request.
- `endpoint_401_when_no_authorization_header` /
  `endpoint_401_when_bearer_mismatch` - the bearer-token middleware
  rejects unauthenticated requests with 401 and a wrong token never
  reaches the handler.
- `endpoint_200_when_bearer_matches` - a valid token returns 200 with
  `Content-Type: text/plain; version=0.0.4` and the body advertises
  every base metric family (`gradient_info`, `gradient_uptime_seconds`,
  `gradient_workers_connected`, `gradient_jobs_pending`,
  `gradient_jobs_active`, `gradient_cache_bytes`).
- `endpoint_reflects_seeded_counts` - the collector renders DB-derived
  counters and gauges with the expected `status` labels (e.g.
  `gradient_builds_total{status="Completed"} 7`).
- `endpoint_rate_limited` - five requests succeed in burst, the sixth
  returns 429 (same tier as `auth_sensitive`).

The metric encoder itself is exercised by a unit test
(`web::endpoints::metrics::tests::render_emits_expected_metric_names_and_help`)
that drives `render` with a fixed `Observations` struct and asserts the
resulting `# HELP` / `# TYPE` lines and value formatting.

## GitHub App as a server-managed integration row

Coverage of the explicit `outbound_integration` opt-in for GitHub status
reporting and the protections around the auto-managed `forge_type=github`
integration rows. Run with
`cargo test -p web --test projects_integration` and
`cargo test -p core --tests ensure_tests`.

`backend/web/tests/projects_integration.rs`:

- `put_project_integration_accepts_github_outbound_row` - linking a project
  to the auto-managed GitHub outbound integration via its UUID succeeds.
- `put_project_integration_accepts_non_github_outbound_row` - Gitea/GitLab
  outbound rows continue to work the same way (regression).
- `patch_integration_rejects_github_row` /
  `delete_integration_rejects_github_row` - server-managed rows can't be
  edited or deleted via the org integrations API.
- `delete_integration_accepts_non_github_row` - non-managed rows still
  delete normally (regression).
- `put_integration_still_rejects_github_forge_type` - POST with
  `forge_type=github` continues to be rejected (no manual rows).
- `put_integration_rejects_reserved_github_name` - name `"github"` is
  reserved for the auto-managed row; user-created integrations using it
  are rejected with 400.

`backend/core/src/ci/integration_lookup.rs::ensure_tests`:

- `creates_both_rows_when_none_exist` - calling
  `ensure_github_app_integrations` on an org with no existing rows inserts
  the inbound and outbound GitHub rows.
- `skips_kinds_that_already_exist` - repeated calls are idempotent; rows
  that already exist for that org/kind are not duplicated.

The resolver simplification (URL-based auto-detection removed) is
indirectly covered by the `put_project_integration_accepts_*` tests:
linking returns the row id and the resolver now branches purely on
`project_integration.outbound_integration` plus the integration's
`forge_type`.

## Worker: empty `built_outputs` self-heals from the parsed `.drv`

Modern daemons can legitimately return `BuildResult::Success` with an
empty `built_outputs` map - fixed-output derivations whose path was
already valid in the local store, for example, get a no-op success.
Older daemon protocols that predate `realisation-with-path-not-hash`
also drain the legacy map into an empty one. Both produce the same
shape on the wire.

`worker/src/executor/build.rs::output_pairs_from_built_or_drv` falls
back to the parsed `.drv`'s declared output paths when `built_outputs`
is empty. Input-addressed and FOD outputs already carry the realised
path in the `.drv`, so the recovery is correct for them. CA / deferred
outputs have an empty `path` until the daemon emits a realisation; if
recovery yields no pairs at all, `realize` returns `Err` so a pure-CA
build without a daemon realisation surfaces as `JobFailed` instead of
being silently recorded with no metadata.

Tests in `executor::build::tests`:

- `output_pairs_use_built_outputs_when_daemon_returned_them` - non-empty
  `built_outputs` is canonical and wins over any `.drv` paths.
- `output_pairs_recover_from_drv_when_built_outputs_empty` - empty
  daemon map, both outputs in the `.drv` carry paths → both recovered.
- `output_pairs_skip_drv_outputs_with_empty_path` - mixed `.drv`
  (one input-addressed, one CA-pending) → only the input-addressed
  pair survives; `realize` keeps the build alive on that single output.
- `output_pairs_returns_empty_for_pure_ca_drv_without_realisation` -
  CA-only `.drv` with no daemon realisation → empty pairs, the caller
  fails the build.

## Worker: eval pushes the runtime closure of every produced `.drv`

Symptom: a downstream build's `prefetch_inputs` failed mid-import with
`add_to_store_nar(...) failed: path '...' is not valid` or
`required input path(s) are missing from local store and not available
in the gradient cache`, even though the path appeared in the cache a
few seconds later. Root cause: `push_drvs` flat-pushed only the `.drv`
file paths in `produced_drvs`, never their `input_sources` (e.g.
`builtins.path` results, `lib.cleanSource` outputs). Those source paths
sat only in the eval worker's local store; the next eval that happened
to produce the same hash incidentally uploaded them and made the user
think it was a "few-seconds-later" cache propagation.

Fix lives in `worker/src/nix/store.rs` and `worker/src/executor/mod.rs`:

- `LocalNixStore::query_references` - returns the daemon's `references`
  for one path; surfaces missing-path / corrupt-connection errors.
- `LocalNixStore::collect_runtime_closure` - BFS over `query_references`,
  visited-set deduplicated. Logs and skips individual unreadable paths
  so one stale path doesn't tank the whole walk.
- `push_drv_closure` (replaces `push_drvs`) - feeds the closure into
  `query_fetched_paths` (CacheQuery Push) and pushes every uncached path
  before `EvaluateDerivations` returns. The eval `JobCompleted` therefore
  arrives at the server only after the cache holds every path any
  downstream build's prefetch will resolve.

The closure walker is exercised end-to-end by the
`nix/tests/gradient/state` fixture (which evaluates a flake and then
builds derivations referencing locally-created sources). Live-daemon
unit coverage isn't worth the harness cost - the BFS is a few lines
and the failure mode is loud.

## Configurable API-key options

`backend/web/tests/auth_hardening.rs` (full HTTP via `axum_test`):

- `api_key_with_only_view_cannot_trigger_evaluation` - a key whose mask is
  `[viewOrg]` returns 403 from the project evaluate endpoint, even when the
  owning user is admin.
- `api_key_pinned_to_other_org_is_invisible` - a key pinned to a different
  org returns 404 (not 403, not 401) on `GET /api/v1/orgs/{name}` so org
  existence isn't leaked.
- `api_key_cannot_create_api_keys` - `POST /api/v1/user/keys` from an
  API-key-authenticated request returns 403 (the self-management guard).

## Frontend access control - `shared/access`

Three API-provided flags drive the user-visible rule that:

- **State-managed** resources (`entity.managed === true`) appear with all
  fields and write buttons visible but disabled, with a hover tooltip
  ("Managed by Nix - edit via declarative config").
- **Read-only** access (`entity.can_edit === false`) hides write buttons
  entirely and shows inputs as disabled with a "You have read-only access"
  hover tooltip.
- **Trigger access** (`entity.can_trigger`) gates trigger-style actions
  (Start Evaluation, Restart Failed Builds, Abort) independently of
  `can_edit`. `can_trigger` reflects `Permission::TriggerEvaluation` while
  `can_edit` reflects `Permission::EditProject`. Backends without the field
  (caches, orgs) cause `accessFromEntity` to fall back to `can_edit`, so the
  existing single-permission model still works.

The primitives:

- `frontend/src/app/shared/access/access.service.ts` - `AccessService` with
  pure helpers (`isWritable`, `shouldShowWriteAction`, `shouldDisableInput`,
  `triggerAccess`). Tests cover the four flag combinations plus
  `triggerAccess` for the deeper model split: it projects an AccessState
  onto trigger-action permissions by replacing `canEdit` with `canTrigger`
  and forcing `managed=false` (trigger actions don't mutate config, so the
  managed flag must not disable them).
- `frontend/src/app/shared/access/writable.directive.ts` - `*appWritable`
  structural directive. Renders content iff `canEdit`. Tests cover
  render/hide on each combination plus toggling.
- `frontend/src/app/shared/access/managed-disable.directive.ts` -
  `[appManagedDisable]` attribute directive. Adds `disabled` and a tooltip
  when `managed || !canEdit`. Tests cover all four flag combinations,
  tooltip text, and that the directive correctly clears its own state when
  access becomes writable. The directive also propagates the disabled state
  through `NgControl.valueAccessor.setDisabledState`, so PrimeNG controls
  (`p-select`, `p-checkbox`, `p-autoComplete`, …) - which only honor the
  ControlValueAccessor hook, not raw DOM `disabled` - actually become
  read-only when the user has no write access. A `FakePrimeNgInputDirective`
  in the spec stands in for any `ControlValueAccessor` host and asserts the
  propagation (issue #229).
- `frontend/src/app/core/resolvers/project-access.resolver.ts` and
  `cache-access.resolver.ts` - fetch the parent entity once, expose
  `{ entity, access }` on `route.parent.data`. Children consume via
  `injectProjectAccess()` / `injectCacheAccess()`. Tests cover the happy
  path and the `managed=true / can_edit=false` propagation. The router is
  configured with `paramsInheritanceStrategy: 'always'`
  (`frontend/src/app/app.config.ts`) so child routes nested under
  `project-layout` / `cache-layout` inherit the `:org` / `:project` /
  `:cache` params from the parent - without it, child components reading
  `route.snapshot.paramMap` get empty strings and the settings pages render
  blank.
- `frontend/src/app/core/services/org-access.service.ts` - derives
  `AccessState` from `Organization.role` and `Organization.managed` for
  org-scoped pages without a parent entity resolver. Tests cover Admin /
  Write / View / undefined / custom-role-name cases.

Each retrofitted feature component (`project-settings`, `project-triggers`,
`project-detail`, `cache-settings`, `cache-upstreams`,
`organization-settings`, `workers`, `cache-subscriptions`, `members-roles`,
`api-keys`, `profile`, `integrations`) carries gating tests covering at
least the two key scenarios:

- **Read-only** (`canEdit=false`): write-action buttons absent from the
  DOM; the page itself remains visible.
- **State-managed with permission** (`managed=true, canEdit=true`):
  config-edit buttons present in the DOM but disabled.

`project-detail` is the exception for trigger-style actions: Start
Evaluation, Restart Failed Builds, and Abort gate on `canTrigger` instead
of `canEdit`. The component exposes a `triggerAccess` computed signal
(`AccessService.triggerAccess(access())`) and the buttons use
`*appWritable="triggerAccess()"` rather than `*appWritable="access()"`. The
spec asserts three scenarios:

- `{ managed=false, canEdit=false, canTrigger=false }` - all three trigger
  buttons absent from the DOM.
- `{ managed=true, canEdit=true, canTrigger=true }` - buttons present and
  enabled (the managed flag does not disable them, because the backend
  permits trigger actions on managed projects).
- `{ managed=false, canEdit=false, canTrigger=true }` - buttons present and
  enabled (a caller with TriggerEvaluation but not EditProject can act).

The two reported bugs that motivated this work are covered directly:

- `WorkersComponent` - Register Worker absent under view-only access;
  present-disabled under state-managed org; per-row buttons honor each
  worker's own `managed` flag independent of the org's access.
- `CacheUpstreamsComponent` - Add Upstream / Edit / Delete absent under
  view-only access (page itself remains navigable); present-disabled
  under state-managed cache.
- `ProjectTriggersComponent` - reporter trigger renders integration
  display name from the inlined `trigger.integration` field (so the
  trigger row shows "from GitHub" rather than the raw `integration_id`
  UUID, even when the caller lacks `ManageIntegrations`); orphaned
  references (`trigger.integration === null`) render "from deleted
  integration".

Run command: `pnpm -C frontend test --watch=false --include='**/access*'`
(primitives) or the full suite with `pnpm -C frontend test --watch=false`.

## Entry-point metrics - surface in-progress evaluations (`web/tests/entry_point_metrics.rs`)

Integration tests for `GET /projects/{org}/{project}/entry-point-metrics`
guarding against the regression where the metrics page renders the empty
state ("No completed evaluations found for this entry point.") even though
the entry-point's build is in a terminal state. The endpoint must filter
only by `project` and `eval`, never by `evaluation.status`, because the
chart's data point is the build's wall-clock - completed builds are
meaningful even while the owning evaluation is still mid-flight.

Three cases:

- `returns_point_when_eval_is_in_progress_but_build_is_completed` - the
  evaluation is `Building` but the entry-point's build is `Completed`. The
  response carries one point with the build's `build_time_ms` and a
  `build_status` of `Completed`.
- `returns_point_when_eval_is_in_progress_but_build_is_substituted` -
  same shape with a substituted build (`build_time_ms = null`). Locks in
  the fix from #119 so a future regression that filters Substituted out
  again is caught.
- `returns_empty_points_when_no_entry_point_matches` - when no
  `entry_point` row matches `project` + `eval`, the response is the empty
  array (the frontend's empty state is then legitimate).

## Entry-point download - pin to newest commit (`web/tests/entry_point_download.rs`)

Integration tests for `GET /projects/{org}/{project}/entry-point-downloads`
guarding against #185: the endpoint previously selected the most recently
*completed* evaluation by ordering on `evaluation.created_at`, so a
retriggered run of an older commit shadowed the latest one. The handler now
resolves against `project.last_evaluation` - the evaluation tied to the
project's newest commit - with no fallback to older evaluations.

Three cases:

- `returns_404_when_project_has_no_last_evaluation` - when
  `project.last_evaluation` is `None` the endpoint 404s instead of running a
  broad search across all evaluations.
- `resolves_entry_point_against_project_last_evaluation` - happy path with
  the project pinned to a newest-commit evaluation; the handler reaches the
  artefact-serving stage (the test stops at empty `derivation_output` rows
  → `404 File`, sufficient to prove the pinned evaluation drove lookup).
- `returns_404_when_entry_point_missing_from_last_evaluation` - when the
  requested `eval` attribute doesn't exist in the newest-commit evaluation,
  the response is 404; no fallback to older evaluations that might still
  carry a matching entry point.

### Cross-cache leader/follower deduplication

- `core/src/db/cache_reach.rs` unit tests cover direct overlap, transitive
  internal chains, external-upstream skip, write-only reader exclusion, and
  cycle tolerance for `writer_orgs_reachable_from`.
- `core/src/db/status.rs::find_active_leaders_tests` covers the cross-org
  match case, the most-advanced/oldest tie-break, the same-org-preferred
  short-circuit, and the cross-org `external_cached` skip.
- `core/src/db/status.rs::reelect_leader_tests` covers same-org promotion
  with cross-org orphaning, and the no-same-org-everyone-orphaned case.
- `scheduler/src/build.rs::cross_org_mirror_tests` covers the pure
  `build_cross_org_artefact_rows` helper: FK rewrite on mirrored products
  and orphan-product dropping.
- `web/tests/cross_org_follower_log_visible.rs` covers cross-org log access
  via `BuildAccessContext::load`'s follower-org fallback.
- `web/tests/evaluation_builds_via_cross_org.rs` covers the
  `GET /evals/{eval}/builds` leader-row swap across organisations.

## Cache local-priority (issue #222)

- `core::types::cli::network::parse_cidr_list` - empty, single IPv4, single IPv6, mixed-with-whitespace, malformed (`banana`), malformed mid-list.
- `core::types::cli::network::in_any` - IPv4 hit, IPv4 miss, IPv6 hit.
- `core::types::config::network_config` - defaults parse; bad `trusted_proxies` and `local_ips` each return errors naming the env var.
- `web::client_ip::resolve_client_ip` - untrusted peer with/without XFF returns peer; trusted peer returns single XFF / first-untrusted-from-right / leftmost-when-all-trusted; malformed XFF entries skipped; all-malformed XFF returns peer; IPv6 happy path; IPv4-mapped IPv6 peer matches IPv4 CIDRs.
- `web::endpoints::caches::narinfo` integration (`cache_local_priority.rs`) - local_priority swapped for matching XFF, not swapped for non-local XFF, ignored when NULL, ignored when 0, ignored when peer untrusted.

## Sentry DSN - operator-overridable reporting target (issue #106)

Tests in `backend/core/src/types/cli/registration.rs` cover the DSN override helper:

- `effective_sentry_dsn_returns_default_when_none` - when `RegistrationArgs::sentry_dsn` is `None`, the helper returns `DEFAULT_SENTRY_DSN` (the upstream Wavelens DSN).
- `effective_sentry_dsn_returns_override_when_some` - when an operator sets `GRADIENT_SENTRY_DSN` / `settings.sentryDsn`, the helper returns the override string, and the three Sentry init call-sites (`backend/src/main.rs`, `cache_loop`, `sign_sweep_loop`) route reports there instead.

## Cache inspection & substituter-compat endpoints (`web/tests/cache_*.rs`)

Integration tests covering the cache inspection and substituter endpoints added
in the cache-inspection feature. All tests use `axum_test::TestServer` driven
against a `MockDatabase` via `test-support` fixtures.

### `?json` flag on text-format endpoints (`cache_json.rs`)

- `nix_cache_info_json_returns_object_with_pascal_case_keys` - `?json` returns `application/json` with `StoreDir`, `WantMassQuery`, `Priority`.
- `nix_cache_info_no_json_returns_text` - without `?json` the content-type is `text/x-nix-cache-info`.
- `gradient_cache_info_json_returns_object` - `?json` returns `GradientVersion` and `GradientUrl` fields.
- `gradient_cache_info_no_json_returns_text` - without `?json` returns key-value text.
- `narinfo_json_returns_object_with_pascal_case_keys` - `?json` on `.narinfo` returns `StorePath`, `URL`, `NarHash`.
- `private_cache_requires_auth` - unauthenticated requests to a private cache return `401` on `nix-cache-info`, `gradient-cache-info`, and `.narinfo`.

### `/nars` cache NAR list (`cache_nar_list.rs`)

- `list_empty_cache_returns_empty_items` - empty cache returns `total = 0` and `items = []`.
- `list_returns_signed_nar_for_cache` - regression for "missing build outputs": a `cached_path_signature` row whose FK resolves MUST appear in the listing. Locks in the SQL JOIN's behaviour so future refactors cannot regress to silently dropping rows that narinfo would still serve.
- `list_private_cache_anon_returns_not_found` - anonymous access to a private cache's listing returns `404`.
- `list_accepts_pagination_query_params` - `page`/`per_page`/`sort`/`order` query params are accepted.

### `/ls/{hash}` NAR tree listing (`cache_narlist.rs`)

- `ls_returns_v1_tree_with_null_offsets` - returns `{ version: 1, root: { type: "directory", entries: { bin: { entries: { hello: { type: "regular", size: 2, narOffset: null } } } } } }`.
- `ls_unknown_hash_returns_404` - unknown hash returns `404`.
- `private_cache_ls_requires_auth` - unauthenticated request to a private cache returns `401`.

### `/serve/{hash}/{*path}` NAR content extraction (`cache_serve.rs`)

- `serve_returns_file_bytes` - `bin/hello` returns raw bytes `"hi"`.
- `serve_returns_tar_zst_for_directory` - `bin/` returns `application/zstd` with zstd magic bytes.
- `serve_unknown_path_returns_404` - non-existent path returns `404`.
- `private_cache_serve_requires_auth` - unauthenticated request to a private cache returns `401`.

### `/log/{drv}` build log (`cache_log.rs`)

- `log_returns_text_for_completed_build_in_cache` - returns `text/plain` log body for a completed build linked to the cache.
- `log_404_when_build_not_linked_to_cache` - `404` when no `cache_derivation` link exists.
- `log_404_when_only_failed_builds_exist` - `404` when only failed builds are recorded.
- `private_cache_log_requires_auth` - unauthenticated request to a private cache returns `401`.

## Log substitution

Tests for when a derivation's outputs are pulled from an upstream cache, Gradient fetches the build log from that upstream as well.

### log_substitution module (`scheduler/src/log_substitution.rs`)

- `dedup_hit_via_existing_log_id_pointer` - newly-inserted Substituted build inherits a sibling's `log_id`.
- `no_prior_build_no_fetch_returns_ok` - without siblings and with `allow_upstream_fetch=false`, returns Ok and leaves `log_id` null.
- `upstream_fetch_persists_log_on_200` - external_cached build's log is fetched from the configured upstream and stored.
- `first_upstream_404_second_200` - falls through to the next upstream on a 404.
- `all_upstreams_404_leaves_log_null` - silent no-op when no upstream has the log.
- `upstream_body_exceeding_cap_is_truncated` - oversize log is capped at LOG_FETCH_MAX_BYTES with a trailing marker.
- `followers_get_log_id_via_backfill` - leader's log_id propagation includes a follower backfill UPDATE.

### Upstream URL helper (`core/src/db/cache_upstream.rs`)

- `returns_urls_from_subscribed_caches` - shared upstream-URL helper.
- `empty_when_no_org_caches` - empty result when org has no caches.

## Derivation path parsing (`core/src/sources/nar_path.rs`)

Unit tests covering `parse_drv_hash_name`, used at scheduler insert time and
on the `/log/{drv}` cache endpoint to split bare `<hash>-<name>.drv` strings
into the `hash` and `name` columns now stored on `derivation` (issue #237):

- `parses_canonical_drv_form` - `aaaa…aa-hello-2.12.1.drv` → (`aaaa…aa`, `hello-2.12.1`).
- `rejects_missing_drv_suffix` - input without `.drv` returns `InvalidPath`.
- `rejects_missing_hash_or_name` - empty hash or empty name returns `InvalidPath`.
- `rejects_missing_dash` - input without `-` returns `InvalidPath`.

## `GET /evals/{eval}/builds` parameter overflow on large evaluations (#237)

The endpoint previously fetched every build in the evaluation and hydrated
display data via `WHERE id IN ($1, …, $N)` against `derivation`,
`derivation_output`, and `build_product`. With ~28 000 builds the combined
parameter count reached 84 560 - over Postgres' 65 535 wire-protocol limit -
and the endpoint returned `500`.

`backend/web/src/endpoints/evals/query.rs::get_evaluation_builds` now chunks
every `IN (…)` lookup at `IS_IN_CHUNK = 10 000` after deduplicating leader IDs
and derivation IDs, and hydrates `has_artefacts` only for the page's
derivations, so the largest single query is bounded by the request `limit`
rather than the evaluation size. The existing mock-DB integration tests
`evaluation_builds_via.rs::{follower_build_is_replaced_with_leader_row,
plain_build_returns_own_row_without_extra_query}` and
`evaluation_builds_via_cross_org.rs` pin the new query sequence; the
behavioural regression for the overflow is verified at code-review time
(every `is_in(...)` site on this hot path is now chunked).

## Worker indirect GC roots for active builds (#245)

Concurrent `nix-collect-garbage` on a worker can otherwise delete a
derivation's inputs (or its just-built outputs before compress+push
uploads them). `GcRootKeeper` (`worker/src/nix/gcroots.rs`) writes one
indirect-root symlink per active build to `GRADIENT_WORKER_GCROOTS_DIR`
(default `/nix/var/nix/gcroots/gradient`); handles remove the symlinks on
drop and the keeper purges the dir at startup to clean up after a
crashed prior worker.

### `gradient_worker::nix::gcroots::tests`

- `disabled_keeper_purge_is_noop` - empty `gcroots_dir` skips startup
  cleanup and never touches the filesystem.
- `disabled_keeper_add_returns_inert_handle` - disabled keeper returns
  a handle that owns no symlink.
- `purge_all_removes_existing_entries_and_creates_missing_dir` -
  startup wipes regular files and symlinks left by a prior crashed
  worker.
- `purge_all_creates_missing_dir` - first-run startup with no dir
  present creates it instead of failing.
- `drop_removes_symlink` - `GcRootHandle::Drop` releases the indirect
  root by removing its symlink.
- `create_symlink_idempotent_skips_existing` - re-adding a root for an
  existing symlink is a no-op (handles cross-build re-entry without
  clobbering the daemon's existing root).

## Frontend titles surface the entity name (issue #229)

`frontend/src/app/core/title/gradient-title-strategy.ts` walks the
`RouterStateSnapshot` for resolved `projectAccess` / `cacheAccess` /
`organizationAccess` data and composes a title of the form
`<entity display_name> · <route title> · Gradient`, falling back to
`<entity> · Gradient` for the root detail pages whose static route title
is implied by the entity itself (`Project`, `Cache`, `Organization`). The
spec at `gradient-title-strategy.spec.ts` covers the four combinations
(both, entity-only, route-only, neither) and the entity-not-found
fallback that keeps the title strategy a noop when an
`organizationAccess` resolver returns `null`.

A companion `organizationAccessResolver`
(`frontend/src/app/core/resolvers/organization-access.resolver.ts`)
fetches the organization for every `/organization/:org/*` route that
doesn't already have a parent resolver. The spec covers the happy path,
the no-param case, and the network-error fallback (resolver must not
fail navigation just because the org fetch errored).

## Evaluation duration parity between project page and log page (issue #229)

`frontend/src/app/shared/evaluation/duration.ts` is the single source of
truth for "how long has this evaluation been running?". Both
`project-detail` and `evaluation-log` now use
`evaluationDuration(evaluation, now)` and `formatEvaluationDuration(ms)`
so the same evaluation row shows the same `Xh Ym Zs` figure on both
pages. The regression that motivated this - the log page kept growing
its duration after the evaluation finished because it used
`Date.now()` instead of `updated_at` - is covered by
`duration.spec.ts`:

- `isRunningEvaluationStatus` - the six in-flight statuses return true,
  the three terminal statuses return false.
- `formatEvaluationDuration` - sub-minute / sub-hour / multi-hour
  rendering, plus the negative-clock-skew clamp.
- `parseUtcTimestamp` - backend timestamps with explicit zone and the
  naive form that gets treated as UTC.
- `evaluationDuration` - terminal evaluations stop at `updated_at`;
  running evaluations track the current time.

## Connector + CLI JSON

The `cli/connector` crate has wiremock-backed unit tests covering each sub-API
(`cli/connector/tests/{auth,user,orgs,projects,evals,builds,build_requests,caches,commits,workers,webhooks,integrations,admin,server}_api.rs`).
Each file covers: happy path, server `{error: true}` envelope (→ `ConnectorError::Api`),
401 (→ `Unauthorized`), and transport failure.

`caches_api.rs::list_caches_decodes_bare_array` is a regression for #290: the
`GET /caches` endpoint returns `message` as a bare array (not the paginated
`{items,total,page,per_page}` envelope used by orgs/projects), so
`caches().list()` returns `ListResponse` (`Vec<ListItem>`). The test replays the
exact response from the bug report to prevent the connector from drifting back to
the paginated type, which made the CLI mis-report a 200 as `api error (200)`.

`cli/connector/tests/client.rs::builder_succeeds_without_system_certs`
guards the rustls trust setup: `Client::builder().build()` must succeed
regardless of whether the platform CA store is reachable. The CLI loads
system certs via `rustls-native-certs` (so self-hosted instances with a
self-signed CA installed in the OS trust store work — fix for #287) and
falls back to the bundled Mozilla CA bundle via `webpki-roots` when no
system store is present (Nix sandbox, minimal containers). Native cert
loading degrades silently when `/etc/ssl/certs` is missing.

CLI integration tests in `cli/tests/`:

- `download_attr.rs` - `gradient download '#attr' --json` writes the right files; `--json` without args returns a structured missing-argument envelope and exits 2.
- `build_watch.rs` (#314) - `gradient build --help` exposes `-b`/`--background` and no longer advertises the removed `--no-stream`; `gradient watch --help` documents the required `<EVALUATION>` argument; `gradient watch` with no argument exits non-zero.
- `completion.rs` - regression for the broken completion bin name: `gradient completion {bash,zsh}` must emit a script that registers against the real `gradient` binary (`-F _clap_complete_gradient gradient`) and never the capitalised `Gradient` app name, which silently disabled `gradient <TAB>`. Also asserts the zsh script appends the autoload bridge (`[[ ${funcstack[1]} = _gradient ]] && _clap_dynamic_completer_gradient "$@"`) so the fpath autoload file the Nix package installs completes on the first TAB instead of only after a second.

Dynamic completer unit tests live in `cli/src/commands/completion.rs` (`#[cfg(test)]`,
wiremock-backed). They drive each completer core against a mock server and assert it
returns the resource names and honours the partial prefix
(`cache_names`, `org_names` reading paginated `items`, `project_names`/`worker_ids`
scoped to the selected org), and that a non-2xx response yields no candidates so the
shell never errors.

Run a single connector test file with `cargo test -p connector --test <name>`; CI runs the full suite.

## PR approval gate (#247)

Untrusted PRs (from forks where the contributor is not a forge repo writer)
are parked in `Waiting + WaitingReason::Approval` instead of running
immediately. Coverage:

- `core/src/types/triggers.rs` - `reporter_pull_request_require_approval_defaults_true_for_legacy_rows`
  asserts the secure-by-default `require_approval = true` decoding for
  pre-#247 rows that lack the field in stored JSON.
- `core/src/types/waiting_reason.rs` - round-trips for the
  `workers` / `approval` / `no_cache` variants plus the legacy-row
  decoder.
- `core/src/ci/apply.rs::no_writable_cache_parks_evaluation_in_waiting_no_cache`
  - `apply_trigger` parks newly-created evals as `NoCache` when the org
  has no writable cache subscription.
- `core/src/ci/apply.rs::no_eval_capable_worker_parks_evaluation_in_waiting_workers`
  (issue #268) - `apply_trigger` parks newly-created evals as
  `Workers { connected_workers: 0 }` when the org has no active worker
  registration with `enable_eval` set. Without this gate the eval
  would sit `Queued` forever - the build-dispatch reconciler only
  acts on builds, which don't exist yet, and the connected-worker
  count alone doesn't distinguish "no eval-capable worker" from
  "build-only worker connected".
- `core/src/db/org_workers.rs::org_has_eval_capable_worker_registration` -
  returns true iff the org has at least one `worker_registration` row
  with `active = true AND enable_eval = true`. Consumed by both the
  park-on-create gate and the unpark short-circuit.
- `core/src/ci/unpark.rs::unpark_no_workers_*` - re-queues evals
  parked with `Workers { connected_workers: 0 }` once an
  eval-capable registration appears in the org; no-op when the org
  still has no eval-capable registration.
- `scheduler/src/build.rs::pre_build_target_queued_no_eval_capable_workers_stalls`
  (issue #268) - regression test confirming the reconciler stalls a
  Queued eval to `Waiting + Workers { connected_workers: 0 }` when
  the eval-capable count is zero, even if total connected workers
  are non-zero (the runtime caller passes the eval-capable count,
  not the total).
- `web/src/endpoints/forge_hooks/events.rs` - extraction of
  `pr_number`, `pr_author`, `is_fork`, `base_owner`, `base_repo` from
  GitHub / Gitea / GitLab payloads.
- `web/src/endpoints/forge_hooks/trigger.rs::parse_gradient_*` -
  recogniser for the `/gradient run [wildcard]` and `/gradient approve`
  PR comments (case insensitive, allows leading quote-reply lines, rejects
  multi-line prose and unknown subcommands, captures an optional trailing
  wildcard string for one-shot overrides; legacy `/ci` prefix is rejected
  by `parse_gradient_legacy_ci_prefix_rejected`). Both commands are
  maintainer-only via `sender_is_trusted`; `/gradient run` falls through
  to `trigger_pr_for_integration` with `manual=true` and the snapshot
  fetched via `CiReporter::get_pull_request` when no parked approval gate
  exists.
- Comment reactions: `CiReporter::add_reaction` (default no-op, implemented
  for Gitea/Forgejo, GitLab, GitHub, GitHub App). The handler fires `eyes`
  immediately after the maintainer trust check passes and `confused` when
  the sender is rejected; the JSON `evaluation.source_comment` column (added
  by migration `m20260527_000000_evaluation_source_comment`) is consumed by
  `core/src/db/status.rs::react_to_source_comment_on_terminal` to post
  `+1` / `-1` once the evaluation hits `Completed` / `Failed` / `Aborted`.
  GitLab's `409 Conflict` (already-reacted) is treated as success so
  repeated terminal transitions don't trip alarms.
- `core/src/ci/unpark.rs::unpark_approval_with_wildcard_*`
  (issue #274) - the new helper writes the maintainer-supplied
  wildcard into the same row update that flips `Waiting -> Queued`,
  so the dispatcher reads a consistent row; same guards as
  `unpark_approval`.
- `core/src/ci/reporter.rs::{gitea,github,gitlab}_comment_url_*`
  (issue #274) - per-forge URL builders for the `post_pr_comment`
  trait method that surfaces wildcard parse errors back to the
  commenter.
- `core/src/ci/reporter.rs::forge_comment_payload_serializes_with_body_field`
  (issue #274) - the shared `{"body": "..."}` JSON payload sent to
  all three forges.
- `core/src/ci/unpark.rs::unpark_approval_*` - transitions
  `Waiting + Approval` back to `Queued` once a maintainer authorises
  the PR; no-ops when the row's reason is something else (NoCache /
  Workers).

## Flake input overrides (issue #259)

### Proto

- `proto/src/messages.rs::flake_input_override_roundtrip` - rkyv roundtrip of a `FlakeJob` carrying `input_overrides`.

### Worker (`backend/worker/src/executor/fetch.rs`)

- `build_archive_argv_appends_override_input_flags` - argv has interleaved `--override-input <name> <ref>` pairs.
- `build_archive_argv_no_overrides_matches_baseline` - empty overrides leaves argv identical to baseline.
- `flake_ref_from_lock_original_github` / `_github_no_ref` / `_indirect` / `_git_url` - flake-ref reconstruction for each `flake.lock` node type.
- `declared_inputs_from_lock_reads_root_inputs` - root inputs are extracted as the declared set.
- `resolve_overrides_keeps_url_some` - explicit URL passes through.
- `resolve_overrides_keep_url_reconstructs_from_lock` - `url = None` resolves via `nodes.<input>.original`.
- `resolve_overrides_unknown_input_drops_with_warning` - unknown input drops and emits warning.

### Web (`backend/web/tests/flake_input_overrides.rs`)

- `list_empty_returns_empty`
- `create_then_list_returns_one`
- `create_with_null_url_keep_url_mode`
- `create_duplicate_input_name_rejects_400`
- `create_invalid_input_name_rejects_400`
- `patch_updates_url_and_returns_new_row`
- `patch_url_to_null_sets_keep_url`
- `patch_omitting_url_does_not_change_it`
- `delete_removes_the_row`
- `list_sorted_by_input_name`
- `get_not_found_returns_404` - also covers cross-project access.
- `managed_project_rejects_mutations_403`

### Frontend (`frontend/src/app/features/projects/project-flake-inputs/project-flake-inputs.component.spec.ts`)

- `lists overrides on init`
- `keep_url checkbox causes url: null submission`
- `non-keep url submission passes url string`
- `edit prefills form fields`
- `delete calls service after confirm`
- `hides write buttons under read-only access`

## Actions (per-project)

### Backend — REST endpoints (`backend/web/tests/actions.rs`)

Run with: `cargo test -p web --test actions`

- `create_send_mail_action_returns_no_token` - POST with `send_mail` config → `201`, `token: null`.
- `create_send_web_request_returns_token_once` - POST with `send_web_request` config → `token` present in response body; subsequent GET returns `token: null`.
- `create_send_mail_without_smtp_returns_400` - when `smtp_enabled=false`, creating a `send_mail` action returns `400`.
- `list_actions_returns_all_project_actions` - GET list includes all created actions, tokens stripped.
- `get_action_strips_token` - GET single action never returns the bearer token field.
- `patch_action_updates_name_and_events` - PATCH changes `name` and `events`; `updated_at` advances.
- `delete_action_removes_row` - DELETE returns `true`; subsequent GET returns `404`.
- `test_fire_creates_delivery_row` - POST `.../test` inserts a delivery row and returns `200`.
- `regenerate_token_returns_new_plaintext` - POST `.../regenerate-token` on a `send_web_request` action returns a new token string.
- `regenerate_token_on_send_mail_returns_400` - calling regenerate-token on a non-`send_web_request` action returns `400`.
- `deliveries_paginated` - GET `.../deliveries` with `limit=2&offset=0` returns first 2 rows; `total` reflects full count.
- `delivery_detail_includes_request_response_body` - GET `.../deliveries/{id}` returns `request_body` and `response_body`.
- `view_role_cannot_delete_action` - `403` for callers without write permission.
- `managed_project_rejects_create` - state-managed projects reject action mutations with `403`.

### Backend — dispatcher unit tests (`backend/core/tests/actions_dispatch.rs`)

Run with: `cargo test -p core --test actions_dispatch`

- `matches_event_returns_true_for_listed_event` - action fires when its `events` list contains the incoming event.
- `matches_event_returns_false_for_unlisted` - action does not fire for events not in its list.
- `matches_event_empty_events_never_fires` - empty `events` list → no dispatch.
- `forge_status_ignores_events_list` - `forge_status_report` always maps `build.started/completed/failed` regardless of `events`.
- `payload_helpers_include_all_fields` - outgoing JSON payload for `send_web_request` contains `event`, `project`, `organization`, `id`, `status`.

### Backend — inline unit tests (`backend/core/src/ci/actions.rs`)

Run with: `cargo test -p core --lib ci::actions::tests`

- `matches_event` - `Action::matches_event` returns correct booleans for listed/unlisted events and the empty-events edge case.
- `forge_status_for_event` - maps `build.started → pending`, `build.completed → success`, `build.failed → failure`; non-build events return `None`.
- `render_subject` - applies `{event}`, `{project}`, `{org}`, `{id}`, `{status}` placeholders to a subject template string.
- `render_default_body` - default body contains event, project slug, entity id, status, and a URL.

### Frontend

Run with: `pnpm --dir frontend exec ng test --watch=false`

- `action-events.component.spec.ts` - checkbox group renders all known event strings; toggling emits updated selection via `ngModel`.
- `action-deliveries.component.spec.ts` - popup renders paginated delivery rows; clicking a row fetches and displays detail bodies.
- `action-form.component.spec.ts` - form switches config fields on `type` change; SMTP-disabled state hides `send_mail` option and shows a warning; one-time token is displayed after create/regenerate and cleared on modal close.
- `project-actions.component.spec.ts` - list page loads actions on init, calls delete service on confirm, links to form modal.

## Cache roles & permissions (issue #265)

- `backend/core/src/permissions.rs` — `CachePermission` bitmask unit tests
- `backend/web/src/access.rs` — `load_cache` access matrix tests
- `backend/web/tests/cache_roles.rs` — role CRUD endpoint tests
- `backend/web/tests/cache_members.rs` — member CRUD endpoint tests
- `backend/web/tests/cache_subscription_gate.rs` — bilateral subscription tests
- `backend/web/tests/cache_api_key_pinning.rs` — cache-pinned API key tests

## Admin tasks & deep GC (issue #271)

- `backend/core/src/db/admin_tasks.rs` — DB helper unit tests: insert/find/mark transitions, unique-violation detection, startup recovery `mark_all_active_failed`.
- `backend/cache/src/cacher/deep_gc.rs` — sweep unit tests: blob pass removes orphan blob, blob pass purges zombie row, log pass removes orphan log, `DeepGcReport` serialises with snake_case keys.

## Evaluation start - surface repository errors (issue #280)

`POST /projects/{org}/{p}/evaluate` and `POST /projects/{org}/{p}/check-repository`
used to swallow git fetch failures (DNS, connection refused, auth) inside
`core::sources::git::check_for_updates` and bubble up a generic 500 with no
actionable detail. The fix propagates the `SourceError` to the web layer, which
maps it to `400 Bad Request` with `code: "repository_unreachable"` and the
underlying git error message. The frontend surfaces that message in an inline
banner under the project header instead of failing silently.

### Backend

Run with: `cargo test -p core --test git_remote`

- `check_project_updates_propagates_unreachable_remote_error` — `git://127.0.0.1:1/…`
  triggers an immediate connection-refused; the helper now returns `Err(SourceError)`
  instead of `Ok((false, vec![]))`. Locks in the propagation guarantee that the
  endpoint relies on for its 4xx mapping.

### Frontend

Run with: `pnpm --dir frontend exec ng test --watch=false`

- `project-detail.component.spec.ts → 'shows an inline error banner when
  startEvaluation fails'` — mocks `ProjectsService.startEvaluation` to throw;
  asserts the `.evaluation-error` banner renders the underlying message.
- `project-detail.component.spec.ts → 'clears the error banner when the user
  retries'` — calling `dismissError()` resets `errorMessage()` to `null` and the
  banner disappears.

## Source-IP allowlist (#282)

### Backend

Run with: `cargo test -p core --test ip_allowlist`

- `empty_list_allows_everything` — empty allowlist is a permissive default so
  existing rows keep working after migration.
- `slash_32_exact_match`, `slash_24_contains_address` — exact-host and net-mask
  containment.
- `ipv4_mapped_ipv6_matches_ipv4_cidr` — dual-stack sockets compare correctly.
- `malformed_entry_is_skipped_but_others_still_count` — validation happens at
  the API edge; the runtime check tolerates noise.
- `normalize_bare_ipv4_to_slash_32` / `normalize_bare_ipv6_to_slash_128` /
  `normalize_keeps_cidr_unchanged` / `normalize_trims_whitespace` /
  `normalize_rejects_garbage` / `normalize_rejects_empty` — write-time canonicalization.

## Upstream cache types + Gradient Proto (#118)

- `cargo test -p entity --lib cache_upstream` — `as_source` for internal/gradient_proto/http + inconsistent rows.
- `cargo test -p core --lib db::cache_upstream` — http vs gradient_proto upstream resolution.
- `cargo test -p core --lib sources::secret` — encrypt/decrypt roundtrip for stored credentials.
- `cargo test -p web --lib endpoints::caches::upstreams` — per-type validation error messages, plus `validate_gradient_proto_requires_https_when_api_key_present` (an API key forces an `https://` upstream) and `validate_gradient_proto_rejects_unsafe_remote_cache` (remote cache name restricted to a safe charset).
- `cargo test -p proto --lib handler::cache` — cache-scoped query + Push rejection.
- `cargo test -p proto --lib handler::cache_session` — read-only message allow-list.
- `cargo test -p proto --lib handler::limiter` — per-IP connection cap.
- `cargo test -p proto --lib handler::cache_consumer` — ws URL building.

## Fetch-capability gating for flake jobs (#252)

A `FlakeJob` carrying a `FetchFlake` task clones its source repository (over
SSH for private repos), and the server only sends SSH credentials to
fetch-capable workers. Assigning such a job to a worker without the `fetch`
capability left it cloning with no credentials callback, failing with
`authentication required but no callback set`. The scheduler now gates these
jobs on the worker's `fetch` capability.

Run with: `cargo test -p scheduler --lib jobs`

- `fetch_flake_job_requires_fetch_capability` — a `FetchFlake` flake job is not
  assigned to a worker lacking `fetch`, but is assigned to a fetch-capable one.
- `cached_eval_job_runs_without_fetch_capability` — an eval-only follow-up job
  (cached source, no `FetchFlake`) still runs on a worker without `fetch`.

## Adaptive fetch/eval split

When an idle dedicated eval worker is connected, the scheduler dispatches a
fetch-only flake job to a fetch worker and hands evaluation to the eval pool via
a cached-source follow-up; a scoring penalty keeps fetch workers free. The eval
worker substitutes the cached source from the binary cache before evaluating.

Run with: `cargo test -p scheduler --lib` and `cargo test -p worker --lib`

- `worker_pool::tests::idle_eval_only_worker_detected` /
  `draining_eval_only_worker_does_not_count` — the split heuristic (an idle,
  non-draining eval-only worker triggers the split).
- `jobs::tests::is_fetch_only_true_only_for_fetch_task_alone` — recognises a
  fetch-only job by its task list.
- `jobs::tests::cached_followup_rewrites_source_and_tasks` — builds the cached
  eval follow-up (Cached source, eval tasks, source as a required path).
- `scheduler_tests::fetch_only_completion_enqueues_cached_eval_followup` — a
  completed fetch-only job enqueues the cached eval follow-up reusing its id.
- `policy::tests::reserve_rule_penalizes_fetch_worker_for_cached_eval_only` —
  fetch workers are penalised for cached-eval jobs, eval-only workers are not.
- `executor::eval::tests::cached_source_requires_store_path_present` — the
  worker substitutes the cached source before eval.

## Forge integration - maintainer approval bypass & wildcard check name (#298)

A fork-PR approval gate must not re-park runs initiated by a maintainer, and a
command-driven run with a custom wildcard must report under its own check line.

The gate decision is split from its forge probe: `decide_pr_gate` resolves
whether the event's actor is a trusted repo writer (via `sender_is_trusted`),
then delegates the branching to the pure `gate_decision`. A `/gradient run`
that creates a fresh evaluation, and a maintainer force-push onto a
contributor's branch (`synchronize`), both thread the event `sender` so the
gate is bypassed once the actor is verified.

The Evaluation check name gains a wildcard suffix
(`gradient/{project}: Evaluation: {wildcard}`) whenever the evaluation's
wildcard differs from the project default.

Run with: `cargo test -p web --lib forge_hooks` and
`cargo test -p core --tests ci::reporting`.

- `trigger::tests::gate_same_repo_pr_bypasses` — same-repo PR runs without a gate.
- `trigger::tests::gate_fork_untrusted_sender_parks` — fork PR with an
  untrusted sender parks for approval (carrying PR number/author).
- `trigger::tests::gate_fork_trusted_sender_bypasses` — a trusted maintainer
  (force-push / command) bypasses the gate.
- `trigger::tests::gate_unknown_fork_status_fails_closed` — uncertain fork
  status with an untrusted sender parks (fail-closed).
- `events::tests::github_pr_sender_distinct_from_author_on_force_push` /
  `gitea_pr_parses_sender_login` / `gitlab_mr_sender_falls_back_to_event_user`
  — the event actor is parsed independently of the PR author.
- `reporting::tests::evaluation_context_format_with_custom_wildcard` — custom
  wildcard produces `gradient/{project}: Evaluation: {wildcard}`.

## Cache upload - NAR ingest, endpoint, connector, and CLI (issue #261)

### Shared NAR ingest (`core::cache::ingest`)

Run with: `cargo test -p core cache::ingest`

- `malformed_store_path_bails_before_any_io` — a syntactically invalid store
  path is rejected before any blob write is attempted.
- `create_path_writes_blob_and_reports_created` — a valid NAR + narinfo pair
  writes the blob to storage and returns `IngestResult::Created`.

### Upload endpoint (`web` crate)

Run with: `cargo test -p web --test caches_upload`

- `upload_unauthenticated_returns_403` — `POST /api/v1/caches/{cache}/nars`
  without a bearer token returns `403`.
- Real-DB integration stubs are present but marked `#[ignore]`; they run in
  CI against a live Postgres instance.

### Connector multipart upload (`connector` crate)

Run with: `cargo test -p connector nar_upload`

- `nar_upload_posts_multipart` — the connector assembles the correct multipart
  form (a `narinfo` JSON part and a `nar` binary part) and maps a 200 response
  to success.

### CLI narinfo parser

Run with: `cargo test -p gradient-cli`

- `parses_full_narinfo` — a complete `.narinfo` file round-trips through the
  parser with all fields populated.
- `missing_required_field_errors` — a narinfo missing a required field (e.g.
  `StorePath`) returns a parse error naming the field.
- `empty_references_ok` — a `References:` line with no paths is accepted and
  produces an empty references list.

### CLI `cache_upload` integration

Run with: `cargo test -p gradient-cli`

- `upload_nar_file_with_narinfo_succeeds` — providing both `--nar-file` and
  `--narinfo` against a mock server returns success.
- `upload_nar_file_without_narinfo_errors` — omitting `--narinfo` in no-nix
  mode exits with a usage error (exit code 2).

### CLI TUI view-model tests

Run with: `cargo test -p gradient-cli`

- `tui::nar_browser` — filter input narrows the displayed list; scroll position
  resets to 0 when the filter changes; clearing the filter restores the full
  list.
- `tui::graph` — expanding a collapsed node adds its children to the visible
  set; collapsing removes them; nested expand/collapse is consistent; `Esc`
  triggers quit.
- `tui::log_view` — `↑`/`↓` scroll adjusts the offset; enabling follow-tail
  pins the view to the last line; `/` search highlights matching lines.
- `tui::watch` (#314) — the `gradient watch` dashboard view-model:
  `BuildSummary::of` classifies build statuses into succeeded/failed/building/
  queued counts; `eval_is_terminal` recognises `Completed`/`Failed`/`Aborted`;
  `format_duration`/`format_build_time` render elapsed and per-build times;
  streamed log chunks split on newlines and buffer partial lines; evaluation
  messages are de-duplicated by id; follow-tail pins to the bottom until an
  `↑` scroll detaches it.

## REST API endpoint surface (NixOS integration)

`nix/tests/gradient/api` boots a single node (gradient + nginx + postgres, no
worker or nix store) and drives `nix/tests/gradient/api/test.py`. Every
management endpoint is hit directly (`curl`) and, where the CLI exposes it, also
through `gradient`. Resources are created at runtime so the creation endpoints
are covered too. Phases:

- **Auth / user / keys** — check-username, register, login, logout; profile,
  settings, sessions, audit-log, search; API-key create/list/revoke/delete.
- **Organizations** — CRUD, available/public, ssh rotation, roles CRUD, and
  membership (a second user is added via `POST /orgs/{org}/users`, re-roled with
  `PATCH`, and removed with `DELETE`, asserting the member list each time).
- **Projects** — CRUD, details, triggers, active toggle, plus a transfer flow
  that moves a throwaway project to a second org and verifies it disappears from
  the source and appears under the destination.
- **Workers** — register/list/patch/delete (direct + CLI), with v4 worker UUIDs.
- **Caches** — CRUD, key/stats, active/public toggles, plus sub-resources:
  member add/re-role/remove, custom-role create/get/patch/delete, an HTTP
  upstream create/patch/delete, and org subscription remove/restore.
- **Cache NARs** — synthetic upload (CLI + direct multipart), list/show/stats/
  available, and delete (CLI plus a direct `DELETE` asserting `204`).
- **Build-dependent endpoints** — exercised on empty state for correct
  not-found behaviour, since no builds are present.
- **Edge cases** — duplicate creates (org, project, cache, org/cache role, API
  key, org/cache member, subscription) return an enveloped `409`; a reserved
  project name (`build-request`) and an empty API-key permission mask return
  enveloped `400`s.
- **Permissions (multi-actor)** — the second user acts with their own token: a
  non-member cannot read the private org; the built-in `View` role grants read
  but is rejected (enveloped `403`) on settings edit, project create, member
  add, and org delete; promotion to `Admin` unlocks the settings edit.
- **State export (`GET /admin/state`)** — rejected (`403`) for a non-superuser;
  after elevating `operator` to superuser in the DB, the JSON format returns the
  seeded org/project/cache with secret `*_file` fields redacted to `null`, and
  the default Nix format renders the same resources as a pasteable expression.

The auth surface is rate-limited (burst 5, one token per 6s), so the script
spaces its registration/login calls to stay within the bucket.

Out of scope (covered by dedicated tests or requiring external services):
OIDC, SMTP e-mail verification, forge webhooks, the worker proto protocol,
the Nix binary-cache serving family, and build-request dispatch.

## State export endpoint (#188)

`backend/core/src/state/export.rs` unit tests cover the secret-redaction pass
(`redact` nulls every `*_file` key at any nesting depth) and the JSON→Nix
renderer (string escaping for `"`, `\`, `${`, and newlines; identifier vs.
quoted attribute keys; nested attrsets/lists; empty `{ }`/`[ ]`; and the header
comment).

`backend/web/tests/admin_state.rs` drives `GET /api/v1/admin/state` through the
router: a non-superuser gets `403`, an unknown `format` gets `400`, and a
superuser over an empty database gets the eight top-level state keys (JSON) and
a `text/plain` Nix body (default). The full round-trip against real data lives
in the NixOS API integration test above.

## Failure handling and retries (#244)

Builds can fail in three distinct ways: `FailedPermanent` (builder exited
non-zero, terminal), `FailedTransient` (OOM / disk full / network error /
builder crash, retried automatically with exponential backoff), and
`FailedTimeout` (wall-clock or silent-output timeout exceeded, terminal).
Server-wide defaults (`GRADIENT_BUILD_MAX_ATTEMPTS`,
`GRADIENT_BUILD_RETRY_BACKOFF_SECS`, `GRADIENT_BUILD_DEFAULT_TIMEOUT_SECS`,
`GRADIENT_BUILD_DEFAULT_MAX_SILENT_SECS`) can be overridden per-derivation
via the `.drv` attributes `timeout`, `maxSilent`, and `preferLocalBuild`.

### `Derivation::build_meta()` parsing — `core/src/db/derivation.rs`

Run with: `cargo test -p core --lib db::derivation`

- `build_meta_reads_all_fields` — all four attributes (`timeout`,
  `maxSilent`, `preferLocalBuild`, `requiredSystemFeatures`) are parsed
  into a `BuildMeta` with the correct values.
- `build_meta_defaults_when_absent` — a derivation with none of the
  attributes returns all-default `BuildMeta`.
- `build_meta_prefer_local_build_accepts_true_and_1` — both `"true"` and
  `"1"` are accepted as `prefer_local_build = true`.
- `build_meta_ignores_unparseable_timeout` — a non-integer `timeout`
  attribute falls back to `None` instead of erroring.

### Build state-machine transitions — `core/src/state_machine/build.rs`

Run with: `cargo test -p core --lib state_machine::build`

- `build_sm_building_to_failed_transient` — `Building → FailedTransient`
  is a valid transition (worker classified the failure as transient).
- `build_sm_failed_transient_to_queued_for_retry` — `FailedTransient →
  Queued` is valid (scheduler re-queues for the next attempt).
- `build_sm_failed_transient_to_permanent_when_exhausted` — `FailedTransient
  → FailedPermanent` is valid (attempt budget exhausted).
- `build_sm_failed_transient_is_not_terminal` — `FailedTransient` is not
  terminal; the state machine permits outgoing edges from it.
- `build_sm_failed_permanent_and_timeout_are_terminal` — `FailedPermanent`
  and `FailedTimeout` are terminal; no outgoing transitions are accepted.
- `build_sm_terminal_failure_rejects_requeue` — attempting to transition
  either terminal failure status back to `Queued` is rejected.
- `build_sm_building_to_substituted` — `Building → Substituted` is valid, so
  a worker that finds the outputs already valid can finalize the build as
  `Substituted` rather than `Completed` (issue #303).

### Retry decision and backoff — `scheduler/src/build.rs`

Run with: `cargo test -p scheduler --lib build::retry_tests`

- `permanent_is_terminal_regardless_of_attempt` — `FailedPermanent` is
  never retried regardless of the current attempt count.
- `timeout_is_terminal` — `FailedTimeout` is never retried.
- `transient_retries_until_budget_then_permanent` — `FailedTransient`
  retries while attempts remain; once the budget is exhausted the outcome
  is `FailedPermanent`.
- `backoff_grows_per_attempt` — the retry delay doubles with each attempt
  (exponential backoff).

### Per-build limit resolution — `scheduler/src/dispatch.rs`

Run with: `cargo test -p scheduler --lib dispatch::limit_tests`

- `per_drv_overrides_default` — a non-zero per-derivation limit takes
  precedence over the server default.
- `zero_means_no_limit` — a stored value of `0` is treated as no limit
  (`None`), not as `0`.
- `falls_back_to_default_when_absent` — when no per-derivation value is
  present, the server default is used.

### Worker failure classification — `worker/src/executor/build.rs`

Run with: `cargo test -p worker --lib executor::build::classify_tests`

- `builder_nonzero_is_permanent` — a non-zero builder exit code maps to
  `BuildFailureKind::Permanent`.
- `oom_signature_is_transient` — a log line matching the OOM heuristic
  maps to `BuildFailureKind::Transient`.

### Entity helpers — `entity/src/build.rs`

Run with: `cargo test -p entity --lib build`

- `is_failure_covers_all_failure_states` — `FailedPermanent`,
  `FailedTransient`, and `FailedTimeout` all return `true` from
  `is_failure()`.
- `terminal_failure_excludes_transient` — `FailedTransient` returns
  `false` from `is_terminal_failure()` (it will be retried); `FailedPermanent`
  and `FailedTimeout` return `true`.

## Closure size graph - issue #242

### Closure builder - `backend/web/src/endpoints/builds/closure.rs`

- `build_closure_graph_sums_and_links` - the closure builder walks the
  dependency closure, sums per-derivation NAR sizes into an exact
  `total_size_bytes`, orders nodes largest-first, and emits one edge per
  in-closure dependency (`source` = dependency, `target` = dependent).

The shared `derivation_closure_reachable` / `sum_output_sizes` helpers (lifted
out of `projects/metrics.rs`) keep their existing coverage in
`backend/web/src/endpoints/projects/metrics.rs` (`sum_output_sizes_*`).

### Shared closure-size helper - `backend/core/src/db/closure.rs`

`transitive_closure_size` is the single source of truth for build-closure NAR
size; both the web closure endpoint and the scheduler's dispatch backfill call
it.

- `sums_closure_output_sizes` - walks a root→child dependency graph and sums the
  coalesced per-derivation output sizes (100 + 40 = 140).
- `empty_roots_is_zero` - an empty root set yields a zero total without touching
  the DB.

The scheduler's lazy backfill (`BuildDispatchMaps::backfill_closure_size`)
computes the size once per derivation when `derivation.closure_size` is NULL,
persists it onto the row, and caches it in the dispatch maps so a dispatch pass
never recomputes; integration coverage rides on the existing dispatch tests,
whose MockDatabase fixtures pre-set `closure_size` to skip the walk.

### Closure Sankey model - `frontend/.../closure-graph/closure-aggregate.spec.ts`

`buildClosureSankey` turns the closure DAG into a flow-conserving tree:

- accumulates each node's subtree size so a node carries its full closure size.
- values links by the dependency's subtree size, pointing dependency → consumer.
- tree-ifies a shared dependency onto a single parent (first reached by the
  breadth-first walk from the roots).
- buckets nodes outside the top `N` into a per-parent `others` node attached to
  their nearest kept ancestor.
- adds no bucket nodes when the whole closure fits within `N`.
- treats nodes unreachable from any root (their consumer was truncated
  server-side) as their own top-level roots.

## Worker host metrics - issue #304

Backend (`cargo test -p worker --bins metrics::`):

- `cpu_core_score_in_bounds_and_positive` — the deterministic single-core
  micro-benchmark (`cpu_core_score`) always returns a value in `1..=100_000`.
- `host_static_reports_nonzero` — `host_static` reports at least one CPU and at
  least 1 MiB of total RAM.

`host_static` (logical CPU count, total RAM) is sampled once and advertised via
`WorkerCapabilities`; `host_dynamic` (available RAM, global CPU usage) is sampled
each heartbeat off the dispatch thread and sent via `WorkerMetrics`.

## History-based prediction - issue #304 (Phase 5.3)

Backend (`cargo test -p scheduler --tests history::`):

- `buckets_are_log2_of_mb` — `closure_bucket` maps closure bytes to a
  log2-of-megabytes bucket (1 MiB → 0, 4 MiB → 2, 1000 MiB → 9).
- `empty_rows_yield_default` — `summarize` over no rows returns the zeroed
  `HistoryPrediction` (samples 0).
- `summarize_aggregates_peak_cpu_and_oom` — peak RAM is the max of non-null
  samples (few-sample fallback for p95), CPU time is the mean of non-null
  samples, and `oom_rate` is the fraction of OOM-killed rows.
- `bucket_bounds_widen_by_one_bucket_each_side` — the byte bounds passed to the
  `derivation_metric` query span ±1 closure bucket around the target size.

`scheduler::history::predict` queries the most recent 200 `derivation_metric`
rows for a `pname` (narrowed to comparable closure sizes when known), index-served
by `idx-derivation_metric-pname-closure_size`. `BuildDispatchMaps` preloads one
prediction per candidate derivation outside the scoring lock; `take_best_of_kind`
feeds each build's prediction to the lazy `history` provider. On build completion,
`BuildStateHandler::record_metrics` inserts a `derivation_metric` row from the
worker's `BuildMetrics` and adopts the worker-measured `build_time_ms`.

## Log compression, chunking, limiting & store-fetch (#246)

Completed build logs are zstd-compressed into line-bounded chunks at finalize and
served lazily by chunk, line range, or streaming search. Workers cap log
throughput with two token buckets and fetch nix-store logs for already-built
derivations.

**`core::storage::sgr` (`SgrState`)** — ANSI SGR carry-forward: `to_prefix()` is
empty for the default state, reconstructs an active foreground colour, clears on
reset (`\e[0m`), combines bold+colour minimally, handles 256-colour sequences,
and ignores an incomplete escape at end of input.

**`core::storage::log_chunk`** — `chunk_log` splits on line boundaries respecting
the byte target, keeps an over-long line whole, carries the active colour as each
chunk's `color_prefix`, and yields no chunks for an empty log.
`compress_and_store_chunks` zstd-encodes each chunk, writes it via `LogStorage`,
and the round-trip (`read_chunk` → `zstd::decode_all`) reproduces the chunk text.

**`core::storage::log` chunk objects** — `write_chunk`/`read_chunk`/`delete_chunks`
round-trip on `FileLogStorage`; `read` reassembles from chunks once the inline log
is dropped (`delete_inline_log`), so full-log reads and dedup keep working.

**`worker::executor::log_limit` (`LogRateLimiter`)** — admits bytes under the
limit, trips permanently on a burst (1-minute bucket exhausted), trips on the
sustained (1-hour) bucket even when the burst bucket would allow, and refills the
minute bucket over elapsed time while not yet tripped.

**`worker::nix::log`** — `store_log_path` computes
`$NIX_LOG_DIR/drvs/<first2>/<rest>.bz2` from a drv store path (and a bare
basename); `read_store_build_log` bzip2-decodes the stored log or returns `None`.

**`web::endpoints::builds::log_chunks`** — `parse_line_range` accepts `start`/`end`,
defaults the start to 1, parses `L120-L130` and bare `3-8`, and rejects malformed
ranges. (The chunk/line/search endpoints' full request/response behaviour is
covered by CI integration tests, not run locally.)

**Frontend `log-window`** — `parseLineFragment` parses `#L`-style deep-link
fragments and rejects garbage/non-positive; `chunkIndexForLine` maps a line to its
chunk index (or `-1`); `windowAround` centres and clamps a fetch window to the log
bounds and handles empty logs. Run a single spec with
`pnpm exec vitest run <file> --globals --environment node`.

**CLI** — `gradient builds log <id>` keeps streaming parity (the server's
`GET /log` reassembles chunks); `--lines L120-L130` fetches a line range and
`--search <term>` streams matches.
