# Tests

This page tracks notable tests added to Gradient and where they live.

## Per-build forge check tracks the whole lifecycle

`backend/gradient-ci/src/reporting.rs`: `build_event_for_status` maps a build
status to the dispatch event the per-entry-point forge check reports.
- `build_event_posts_live_progress` - `Queued` -> `build.queued` (Pending),
  `Building` -> `build.started` (Running), `Completed`/`Substituted` -> their
  terminal events, so the `Build {label}` check tracks progress instead of only
  appearing once the build is done.
- `build_event_dependency_failure_and_abort_are_failures` - `FailedPermanent`,
  `FailedTimeout`, `DependencyFailed`, and `Aborted` all report `build.failed`
  (which `forge_status_for_event` maps to `Failure`), so a dependency-failed or
  aborted entry point shows a failed check rather than a check stuck on Pending.
- `build_event_skips_created` - the initial `Created` state posts no check.

Queued/dependency-failed/aborted transitions run as bulk SQL updates that bypass
`update_derivation_build_status`; `notify_build_status_for_derivations`
(`backend/gradient-db/src/status/derivation_build_status.rs`) fires the reactor
for the affected entry points so those events still reach the forge.

## NAR writes are idempotent (no redundant re-uploads)

`backend/gradient-proto/src/ingest.rs`: `put_nar_idempotent` skips the
object-store write when an identical NAR is already stored, so re-pushing
unchanged content is a metadata-only no-op instead of a fresh `PUT` (which on a
versioning-enabled bucket would accumulate retained versions no S3-API GC can
reclaim).
- `idempotent_skips_when_present_and_hash_matches` - a recorded matching
  `file_hash` plus the object on disk skips the write and leaves the bytes
  untouched.
- `idempotent_writes_when_no_row` / `idempotent_writes_when_hash_differs` - a
  first write, or a changed `file_hash` (non-reproducible rebuild), always
  writes so stale bytes are never served.
- `idempotent_writes_when_object_missing` - a matching row whose object is gone
  (zombie) re-writes, restoring the row⟺object invariant.
- `backend/gradient-storage/src/nar.rs`: `exists_reflects_presence` covers the
  `HEAD` guard. The guard is server-side only; worker→S3 presigned uploads
  bypass it, so the NAR bucket must not retain noncurrent versions.

## S3 build logs are not cached on local disk

`backend/gradient-storage/src/log.rs`: `s3_chunks_are_not_cached_on_local_disk`
asserts `S3LogStorage::write_chunk` writes finalized log chunks only to the
object store, not to the local file store, so an S3 backend keeps no build logs
on local disk at rest (the live log still appends locally during a build and is
dropped on finalize). Reads fall back to S3.

## Evaluation GC keeps failed/aborted NARs and defers during active runs

`backend/gradient-db/src/gc.rs`: `skips_gc_while_an_evaluation_is_active`,
`retains_keep_most_recent_terminal_regardless_of_outcome`,
`keeps_single_terminal_within_keep`, and `deletes_terminal_evaluations_beyond_keep`
cover `evaluations_to_gc` after it stopped deprioritizing `Aborted` runs and
started skipping the whole project while any evaluation is active. Previously GC
preferentially deleted failed/aborted evaluations (dropping the completed NARs
they held) and could race an in-flight run.

## GitHub App installation org-binding

`backend/gradient-web/src/endpoints/forge_hooks/trigger.rs`:
`github_full_name_parses_every_url_form`, `github_full_name_rejects_non_github_hosts`,
and `installation_payload_collects_full_names_from_both_arrays` cover the binding
of an `installation` / `installation_repositories` webhook to every org owning a
project whose repository URL resolves to one of the payload's repositories. The
match is purely on the parsed `owner/repo`, so the flake shorthand
(`github:owner/repo`) binds the same as the https / SSH clone URLs; previously the
matcher keyed off a literal `github.com` substring and an org-name equals login
fallback, so a `github:`-form project (and any org not named after the account)
left the org without a `github_installation` row.

`github_installation.created_at` must be `TIMESTAMP` to match the entity's
`NaiveDateTime`; the create migration originally typed it `TIMESTAMPTZ`, so every
read (e.g. `PUT /orgs/{org}/integrations`) failed to decode the column (#449).
`m20260620_000004` converts the column in place on already-migrated installs
(conditional on the current type, so it is a no-op once `TIMESTAMP`). Verified by
E2E CI against real PostgreSQL; sea-orm `MockDatabase` cannot reproduce the
column-type decode error.

`event_repo_matches_project_is_host_agnostic_on_owner_repo`,
`event_repo_rejects_a_sibling_repo_in_the_same_org`,
`event_repo_empty_urls_match_every_project`, and
`event_repo_unparsable_project_never_matches` cover the fan-out repo gate: an
org-wide inbound integration (a GitHub App installation spans every repo in the
org) must fire only triggers whose project tracks the repository the webhook
event came from. Matching is host-agnostic on `owner/repo`. Without it a push/PR
to one repo fanned out to sibling projects, whose eval carried the wrong repo's
commit and made the reporter post a check-run for a SHA absent from the project's
repo (GitHub 422 "No commit found for SHA", #449).

## Per-forge webhook events guidance (Gitea/Forgejo/GitLab `/gradient` commands)

`frontend/.../integrations/integrations.component.spec.ts`: two specs assert
`requiredWebhookEvents` lists the events the server actually classifies - Gitea/
Forgejo get `Issue Comment` + `Pull Request Review` (and never push-only), GitLab
gets `Merge request` + `Comments (note)`. A push-only forge webhook never
delivers the comment/note or PR/review events, so `/gradient run` / `/gradient
approve` and PR CI silently never fire on these forges (unlike the GitHub App,
whose manifest auto-subscribes to `issue_comment`/`pull_request_review`); the
inbound-integration card and the setup docs now surface the full event set.

## Forgejo webhook header support (#460)

`backend/gradient-forge/src/providers/gitea.rs`:
- `accepts_forgejo_and_gitea_headers` - the Gitea/Forgejo provider lists both
  `X-Forgejo-Event`/`X-Forgejo-Signature` and the Gitea-prefixed equivalents, so
  a Forgejo host (e.g. Codeberg) that sends only its own header family is still
  classified and signature-verified instead of falling through to a silent
  `Unknown`-event 200. The signature check runs before event classification, so
  a missing header family previously dropped every webhook.
- `classifies_pr_comment_as_comment` - `issue_comment` / `pull_request_comment`
  map to `WebhookEventKind::Comment`.

When a `/gradient` comment is delivered but the integration has no project with a
usable forge reporter (no API token), `handle_issue_comment` now logs a `warn`
instead of silently returning, making the "nothing happened" case diagnosable.

## GitHub App setup cleanup + state surfacing + breadcrumbs (#441, #452)

Frontend (`pnpm --dir frontend exec ng test --include='**/eval-status-badge.component.spec.ts' --watch=false`):
- shared `EvalStatusBadgeComponent` collapses `Evaluating*` to `Evaluating`,
  applies the success class for `Completed`, spins a running icon, and pulses a
  queued icon. Reused by the organization detail list and the project detail
  title (one source of truth for the eval status tag).

Frontend (`**/project-detail.component.spec.ts`):
- `disables Start Evaluation while an evaluation is in progress` and `enables
  Start Evaluation once the latest evaluation finished` - the button is gated on
  `evaluationInProgress()` so it no longer surfaces "evaluation already in
  progress".
- `shows the eval status badge in the title while in progress` / `hides the title
  badge when the latest evaluation is terminal`.

Frontend (`**/project-actions.component.spec.ts`):
- `includes a Settings link in the breadcrumb` - Actions breadcrumb is now
  `[Org] / [Project] / Settings / Actions`.

## Forge action "Test" button connectivity probe

`backend/gradient-forge/src/reporter.rs`: `verify_reads_repo_without_reporting`
asserts `CiReporter::verify` performs a non-mutating repo read (the default
branch) and never calls `report`, and propagates a forge error when the repo is
unreachable. This backs the Actions Test button, which now probes connectivity
for `forge_status_report` / `open_pr` instead of posting a status against a
placeholder commit (which the forge always rejects).

## Minor frontend issues (#401)

`backend/gradient-web/src/endpoints/projects/auto_attach.rs`: `host_parsing_covers_url_shapes`,
`self_hosted_pairs_inbound_and_outbound`, `public_github_matches_by_forge_type`,
`ambiguous_inbound_is_skipped`, and `unrelated_forge_does_not_match` cover the
repository-URL → org-integration matcher that auto-attaches a push trigger and a
status-report action on project creation.

`backend/gradient-forge/src/webhook.rs`: `github_push_extracts_commit_subject_and_author`,
`github_push_without_head_commit_has_no_message`, and `gitlab_push_picks_commit_matching_after`
cover push webhooks now writing the commit subject + author (previously only Pull/PR triggers did).

## Draining server + board fixes (#411)

`backend/gradient-types/src/waiting_reason.rs`: `draining_round_trip` asserts the
new `WaitingReason::Draining` variant serialises to `kind: "draining"` and decodes
back.

`backend/gradient-db/src/draining.rs` (all on `MockDatabase`):
`park_returns_rows_affected`; `unpark_touches_only_draining_parks` (a `Draining`
park is recovered to `Queued` while a sibling capacity park is left untouched);
`unpark_skips_update_when_no_draining_parks` (no UPDATE issued when nothing is
parked).

`backend/gradient-worker/src/worker_pool/eval_stats.rs`:
`stats_env_enables_nix_show_stats_only_when_metrics_on` covers the eval-worker env
that turns libnixexpr's thunk/function-call counters on (`NIX_SHOW_STATS`), the fix
for thunks/fn-calls always reporting `0`.

`frontend/src/app/features/board/health/health.component.spec.ts`: draining
controls render "Enable Draining" with no banner when idle, "Disable Draining" with
a banner when draining, and the button click calls `AdminService.setDraining(true)`.

## Base workers (#115)

`backend/gradient-db/src/` - `base_worker_db_helpers` tests cover inserting and querying `base_worker` rows, the `eval_gate` fallback (base workers bypass the per-org eval gate when `authorize_against` is set), and the `enabled` global flag filtering.

`backend/gradient-proto/` - `base_worker_auth` tests cover the auth-challenge path for a base worker: wildcard `*` handshake succeeds, per-org scope is rejected when `authorize_against` forces a fixed identity, and a base worker with no connected orgs is rejected with an appropriate error.

`backend/gradient-state/` - `state_validation` tests that a non-base worker with an empty `organizations` list fails validation, and that `base_worker = true` with an empty list is accepted. `state_provisioning` asserts that pre-enabled orgs listed under a base worker get `worker_registration` rows at provision time.

`backend/web/` - `list_workers_includes_is_base` asserts the union query marks base-worker rows `is_base: true`; `patch_enable_disable_base_worker` verifies that org members can toggle a base worker on/off but PATCH with `display_name` or `enable_fetch` is rejected (405); `fire_test_endpoint` tests the `POST /orgs/{org}/workers/{id}/test` response shape for connected/disconnected and authorized/unauthorized states.

`frontend/src/app/organizations/workers/` - `workers.component.spec.ts` covers rendering of `is_base` badge, the enable/disable toggle emitting the correct PATCH body, and that the edit/delete actions are absent for base-worker rows.

`nix/tests/gradient/api` - E2E NixOS VM test provisions a base worker via state, calls `GET /orgs/{org}/workers` and asserts `is_base: true` in the response, then calls `POST /orgs/{org}/workers/{id}/test` and checks `ok: true`.

## State export includes base workers (#405)

`backend/gradient-state/src/export.rs` - `export_base_worker_emits_flag_orgs_and_authorize_against` asserts the exporter reconstructs a base worker as a `StateWorker` with `base_worker = true`, the `enabled` and `enable_*` gates, the stringified `authorize_against` UUID, and only the orgs linked via `organization_base_worker` (other base workers' links excluded); `token_file` stays blank because it cannot be recovered from the stored hash.

## Scoring rule descriptions (#403)

`backend/gradient-score/src/policy.rs` - `rule_catalog_covers_every_rule_with_a_description` asserts every `ScoreRule` in the superset policy appears once in `rule_catalog()` with a non-empty name and description, guarding against a new rule shipping without help text.

`frontend/src/app/core/services/board.service.spec.ts` - `getScoringRules()` test verifies the catalog is fetched from `board/scoring/rules`, unwrapped from the response envelope, and cached so repeat subscribers do not refetch.

## Worker CPU/RAM saturation penalty

`backend/gradient-score/src/rules/resource.rs` - `ResourceSaturationRule` applies `-1000` to a real build dispatched to a worker whose live CPU usage is `>= 80%` (`>= 90%` for substitute-only `builtin` fetches) or whose free RAM is `<= 10%` of total, plus another `-1000` when the build's historical peak RAM x1.1 exceeds the worker's free RAM. Both stay below the `WaitTimeRule` cap so anti-starvation can still win eventually.
- `saturation_penalizes_real_build_on_hot_cpu_or_ram_only` - a non-`builtin` build scores `-1000` on a CPU-hot or RAM-hot worker and `0` on an idle one.
- `saturation_is_lenient_for_builtin_and_exempts_evals_and_no_metrics` - at `85%` CPU (between the two thresholds) a `builtin` fetch scores `0` while a real build scores `-1000`; eval jobs and workers reporting no metrics score `0` regardless of saturation.
- `ram_prediction_exceeding_free_penalizes_and_stacks_with_saturation` - a build whose predicted peak RAM x1.1 exceeds free RAM scores `-1000`, `0` when it fits, and `-2000` when the worker is also saturated.

## Wildcard attr tolerance + fair-share idle gate (#419)

A wildcard (`*`/`#`) legitimately spans attrs that aren't buildable derivations, so a drvPath-resolution failure on a wildcard-matched attr is skipped silently; only an attr the user pinpointed exactly still surfaces the error. The `FairShareRule` penalty now applies only when every worker is busy, so a lone busy org is never penalized below the dispatcher's zero floor into leaving the cluster idle.

- `backend/gradient-worker/src/executor/eval.rs`: `explicit_attr_set_keeps_only_wildcard_free_includes` asserts only wildcard-free, non-exclusion patterns count as explicit (quoted dots collapse to the discovered path form).
- `backend/gradient-score/src/rules/fair_share.rs`: `idle_capacity_lifts_penalty` asserts a busy org is rationed when `idle_workers == 0` but not penalized when a worker is idle.

The scheduler also keeps a bounded ring of recent dispatch decisions - every scored candidate, including the rejected/negative ones the dispatcher passed over - exposed at `GET /board/jobs/decisions` (superuser-only) and surfaced by a Live Jobs "incl. rejected" dropdown so operators can see all scores while tuning rules.

- `backend/gradient-scheduler/src/jobs.rs`: `records_dispatch_decisions_including_rejected_candidates` asserts a worker that idles on a negative best score still records the decision with the rejected candidate and its negative score, and a later dispatch records a decision naming the winner.
- `backend/gradient-scheduler/src/jobs.rs`: `terminal_job_removal_prunes_scores_across_all_workers` / `aborting_an_evaluation_prunes_its_pending_scores` guard the per-worker score map against the dispatch leak - completing, aborting, or eval-aborting a job now drops its recorded scores on every worker, so `take_best_of_kind`'s per-dispatch lookup stays bounded instead of growing with total dispatched jobs.
- `frontend/src/app/features/board/live-jobs/live-jobs.component.spec.ts`: the decision-scores spec asserts the "incl. rejected" scope flattens decisions to candidate rows, marking the winner and showing negative-scored, passed-over candidates.
- `backend/gradient-web/tests/board_decisions.rs`: `dispatch_decisions_rejects_non_superuser` and `dispatch_decisions_superuser_returns_empty_ring` guard the routing-tier fix - the handler needs `Extension<MUser>`, so the route must sit on the authenticated tier; on the optional-auth tier it returned `500` for every caller and the "incl. rejected" table stayed empty.

## Bulk evaluation abort + dispatch race

Aborting an evaluation parks it as `Waiting` with `WaitingReason::Aborting` before touching its builds, then aborts the eval's in-flight `derivation_build` anchors in a handful of set-based statements. Because anchors are global, an anchor is aborted only when no other non-terminal evaluation still needs it (a `build_job` in another live eval keeps it running); the dispatcher's queue finder skips `Waiting` evaluations meanwhile.

- `backend/gradient-types/src/waiting_reason.rs`: `aborting_round_trip` asserts `WaitingReason::Aborting` serialises to `kind: "aborting"` and decodes back.
- The anchor-abort SQL (set-based, shared-anchor guarded) is verified by E2E CI, not a MockDatabase sequence test.

## Worker shadows base worker of same id (#407)

`backend/gradient-web/src/endpoints/orgs/workers.rs` - `registration_shadows_base_worker_of_same_id` asserts `unshadowed_base_workers` drops a base worker whose `worker_id` matches one of the org's normal registrations, so `GET /orgs/{org}/workers` lists the conflicting worker only once (as the org registration). `delete_org_worker` deletes the registration first, so the normal worker is removed even when a base worker shares its id, and the `409` base-worker guard fires only when no registration exists.

## Global derivation build identity (build-once anchors)

A derivation is built exactly once across all evaluations and organisations.
State lives on a `derivation_build` anchor (1:1 with the content-addressed
`derivation`, `UNIQUE(derivation)`); each evaluation links to it through a
per-eval `build_job`, and `Created → Queued` promotion is driven by the global
`derivation_dependency` graph, decoupled from any single evaluation's
completion (this is what fixes builds stuck in `Created` behind a
never-completing eval). Promotion (`promote_ready`/`promote_dependents`) and
`dispatch_ready_builds` are gated on reachability - an anchor is queued and
dispatched only while some `build_job` references its derivation - so the
per-derivation anchors seeded by `m20260619_020000` are never queued or
dispatched without a driving evaluation (which previously logged "no driving
evaluation for anchor"). The probe is backed by the `idx_build_job_derivation`
index (`m20260620_000003`).

The build-once guarantee is enforced by the DB `UNIQUE(derivation)` constraint
plus `INSERT ... ON CONFLICT (derivation) DO NOTHING` in
`scheduler::eval::resolve_anchors`, not by a MockDatabase test (which cannot
exercise a real `ON CONFLICT`). The promotion SQL
(`gradient_db::promotion::{promote_dependents, promote_ready,
cascade_dependency_failed}`), its reachability gate, and the reachability
refcount used for access and GC (`gradient_db::reachability`) are covered by
E2E CI against real PostgreSQL.
Worker-side, `backend/gradient-worker/src/executor/eval.rs` -
`pushes_batch_closure_before_reporting_it` still guards that each batch's `.drv`
runtime closure is pushed to the cache before the server promotes and dispatches
that batch, so a build never prefetches a source the cache does not yet hold.

The `m20260619_010000_globalize_derivation` migration collapses duplicate
`(hash, name)` derivations onto the lowest-id survivor and re-points every FK.
Every re-pointed junction table that carries a `UNIQUE` index over the
derivation column - `derivation_output`, `derivation_dependency`,
`derivation_closure`, `derivation_feature`, `cache_derivation` - must drop that
index before re-pointing and rebuild it after collapsing the duplicate pairs;
omitting one (e.g. `derivation_feature (derivation, feature)`) makes the
re-point `UPDATE` violate the constraint when a duplicate and its survivor share
a row. Verified by E2E CI applying the migration against real PostgreSQL.

## PostgreSQL minimum-version guard (#387)

`connect_db` reads `server_version_num` at startup and aborts before running
migrations when the server is older than PostgreSQL 18, which the metric/stats
rollups require (`uuidv7()`). `backend/gradient-db/src/connection.rs::pg_version_tests`
covers the pure decision: `170_004`/`179_999` are rejected (with a `17.4`-style
detected-version message) and `180_000`+ are accepted.

## Migration start logging (#446)

`run_migrations` in `backend/gradient-db/src/connection.rs` logs the pending
migration count and names at `info` before calling `Migrator::up`, then logs
completion, so a slow schema change is not mistaken for a hung process. No
applied migrations logs `database schema up to date`. The behaviour depends on
the live `seaql_migrations` table (after `install`/prune) so it is exercised at
server startup, not via the MockDatabase unit harness.

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

- `backend/gradient-db/src/permissions.rs` - declares the [`Permission`] capability
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

## Off-loop RPC dispatch (CacheQuery head-of-line blocking)

Both the server's per-connection loop (`gradient-proto/src/handler/session.rs`) and the worker's dispatch loop (`gradient-worker/src/worker/dispatch.rs`) process frames serially, so a slow handler (an inline `Pull` upstream narinfo probe, a slow object-store `PUT`) head-of-line-blocked every other message and starved concurrent `CacheQuery`s into their 120 s `CacheStatus` deadline; a substitute that missed the deadline then escalated into a needless from-scratch build. The fix spawns the order-independent handlers off both loops (`CacheQuery`, `QueryKnownDerivations`, `WorkerMetrics` on the server via `RpcContext`; the presigned NAR upload on the worker), keeping order-sensitive handlers (NAR push chunks, log appends) inline.

This is a concurrency property of the connection loops, not a pure-function behavior, so it is covered by the existing proto round-trip tests (handler logic is unchanged) plus E2E CI rather than a new MockDatabase unit test, which cannot observe loop scheduling. `decide_dispatch_mode` (the escalation that this prevents from firing on transient timeouts) keeps its unit tests in `gradient-scheduler/src/dispatch_mode.rs`.

## CacheQuery DB error is indeterminate, not "absent"

Under a large eval the shared scheduler/cache DB pool exhausted (`Connection pool timed out`, 8 s acquire), and the `CacheQuery` local-cache lookups (`build_local_cache_map` / `load_cached_path_rows`) swallowed that error into an empty result - so a *fully-cached* input was reported `cached: false`, which the worker took as a terminal `InputsUnavailable` and failed the whole eval. The lookups now propagate their `DbErr`; `query` short-circuits and the handler replies `CacheError { job_id, message }` (a new `ServerMessage` variant) instead of a `CacheStatus`, so the worker resolves the prefetch as a transient transport failure and retries. `backend/gradient-proto/src/handler/cache.rs`: `cache_query_propagates_db_error_as_err` drives the lookup from a `MockDatabase` seeded with `append_query_errors` (via `test_state_cache`, which routes the test DB into the dedicated `cache_db` pool) and asserts `query(... Pull)` returns `Err`, not a confident uncached list. The server-side budget (`CACHE_QUERY_BUDGET`) and dedicated `cache_db` pool are concurrency/timeout properties covered by E2E CI.

## edges_complete must not be set over a dropped edge

`flush_deferred_deps` resolves each reported `(source, [dependency])` edge to recorded derivations; a dependency the eval never persisted (interrupted/overlapping eval) was silently dropped, but `mark_edges_complete_for_eval` still marked the source `edges_complete` via its "is a build_job" branch - so a build_job that *declared* a dependency but recorded zero edges was dispatched as dependency-free and failed `InputsUnavailable` on an input the server had no record of. A new `derivation_build.edges_unresolved` flag (migration `m20260627_000000`) records "this anchor's declared edge set is incomplete"; `flush_deferred_deps` sets it for sources with an unresolvable dep and clears it once they all resolve, and `mark_edges_complete_for_eval` refuses to promote a flagged anchor. `backend/gradient-scheduler/src/eval.rs`: `deferred_edges_flag_sources_with_unrecorded_deps` asserts the pure `resolve_deferred_edges` flags a source with an unrecorded dependency (and excludes it from the resolved set) while a fully-resolved source is the opposite. The `mark_edges_complete_for_eval` SQL guard (`AND NOT db.edges_unresolved`) is covered by E2E CI (MockDatabase can't run the recursive CTE).

## Dispatch gates on the input-`.drv` closure being cached

The eval pushes `.drv`s progressively as it resolves the graph, but a build worker cannot even import a build target's `.drv` until that `.drv`'s full reference closure (every transitive input `.drv` plus its input sources) is in our cache - the nix daemon's `add_to_store_nar` rejects a NAR whose declared references are absent. The dispatch gate previously checked only dependency OUTPUTs (`closure_complete`) and input sources, so a build could be dispatched before the eval finished pushing its `.drv`s and fail terminal `InputsUnavailable` on a `.drv` (its own, a dependency's, or a transitive one) - the dominant failure mode of large NixOS system-closure derivations (`etc`, `system-units`, `activate`). A new `derivation_build.drv_closure_cached` flag (migration `m20260629_000000`) is the `.drv`-closure analogue of `closure_complete`: `reconcile_drv_closure_cached` (gated on `edges_complete`) marks an anchor once its own `.drv` is in `cached_path` and every build dependency is itself `drv_closure_cached`, run in the 5 s dispatch tick, at eval completion, and during graph-unstick. `dispatch_ready_builds` (and the non-substitutable branch of the promote gates) require `db.substitutable OR db.drv_closure_cached`; substitutable anchors substitute their output and never import their `.drv`, but their dependents still require it via the recursive gate. The fixpoint and SQL gate are covered by E2E CI (MockDatabase can't run the recursive marking).

## Transient substitute failures must not escalate

A substitutable build whose relay fails on a transient timeout (the CacheQuery Pull RPC, the upstream NAR download, or the presigned PUT into our own store) was reported as `SubstituteUnavailable`, which counts toward the miss-escalation threshold - so two unlucky timeouts escalated the build into a from-scratch one. The worker now reserves `SubstituteUnavailable` for a genuine "not on any upstream" miss (typed `SubstituteNotOnUpstream`) and reports everything else as retryable `Transient` (which carries no `attempt_reason`, so it doesn't count). `backend/gradient-worker/src/executor/mod.rs`: `substitute_failure_classification` asserts a `SubstituteNotOnUpstream` error (raw and `.context()`-wrapped) maps to `SubstituteUnavailable`, while a timeout error maps to `Transient`. The probe-permit acquire in `gradient-core/src/upstream.rs` is also bounded (`PERMIT_ACQUIRE_TIMEOUT`) so a saturated query pool can't make a probe block past the worker's 120s `CacheStatus` budget.

## Upstream NAR proxy forwards the query string

The cache re-serves a path it found on an external upstream by rewriting the narinfo `URL` to `nar/upstream/{upstream_id}/{upstream_url}`, preserving the upstream's own query (e.g. hash-routed caches like `cache.nixos-cuda.org` resolve a NAR only with `?hash=<storehash>` and 404 "missing outhash" without it). But `upstream_nar` captured only the `{*path}` segment and `fetch_upstream_nar` built `{base_url}/{path}`, dropping the request query - so the proxy fetched the upstream NAR without `?hash=` and 404'd ("NAR in upstream not found") a NAR the upstream actually has, which surfaced downstream as a substitutable build failing `upstream reported … but the NAR object is missing` and cascading `DependencyFailed`. `upstream_nar` now also extracts `RawQuery` and forwards it. `backend/gradient-web/src/endpoints/caches/nar.rs`: `upstream_nar_url_forwards_query_string` asserts `build_upstream_nar_url` appends a non-empty query and leaves a `None`/empty one off.

## Demote and keep-set must agree with the prune predicate

Two latent bugs let a real build be dispatched for a derivation whose `.drv` wasn't in the cache, surfacing as `input prefetch failed … required input path(s) are missing` on the build's own `.drv`.

- `demote_cached_output` cleared `is_cached` but left `external_url`, while resetting the producer to a real build. Pruning (`prunable_known_derivations`) keys on `external_url`, so the node stayed pruned, was never re-walked, its `.drv` never re-pushed, and the reset-to-build anchor dead-ended. `backend/gradient-db/src/cache_storage.rs`: `demote_clears_upstream_availability` asserts `demoted_output` sets `external_url`/`nar_hash`/`file_hash`/`file_size` to `None` alongside `is_cached`/`cached_path`, so `is_cached_anywhere()` is false and the next eval re-walks the node.
- The orphan-files keep-set (`active_hashes`) gated the `.drv` and input-source clauses on build status, so a terminal-failed-but-requeueable anchor lost its (producerless) `.drv`. `backend/gradient-cache/src/cacher/cleanup.rs`: `keep_set_protects_drv_and_sources_for_any_anchor` asserts only the outputs clause carries `b.status NOT IN (...)` (outputs are rebuildable / TTL-evicted) while the `.drv` and input-source UNIONs keep the closure for any anchor.

Both are shape/pure-function tests (no SQL execution): MockDatabase can't run the queries, so coverage mirrors the proven SQL + the `ActiveModel` field-clearing, with E2E CI exercising the live path.

## Concurrent same-path NAR pulls don't share a partial file

On a local-storage (non-S3) server, NARs stream to the worker as chunked `NarPush` frames staged to a `.partial` file. The worker keyed that file by the bare store-path hash, so two builds on one worker pulling the *same* input (e.g. `glibc-locales`, `docbook-xsl`) appended to one shared file: interleaved offsets failed `partial append failed: non-contiguous … offset 8388608 != len …` and corrupted the staged NAR (S3 pulls bypass staging via presigned HTTP, so they were unaffected). The pull partial is now keyed `{job_id}/{hash}`, mirroring the server-push `{peer_id}/{hash}` namespacing. `backend/gradient-worker/src/proto/nar_recv.rs`: `concurrent_jobs_same_path_do_not_collide` interleaves two jobs' chunks for one path and asserts both assemble correctly; `partial_store_resumes_across_reconnect` still covers per-job resume. No protocol change (`NarRequest`/`NarPush`/`NarRequestResume` are unchanged).

## Orphan-files GC spares freshly-written NARs (upload-vs-GC race)

A first-run eval (`019f15bf`) failed `MissingInputs` on `.drv`s with `drv_closure_cached=true` while the `.drv`s were absent from the cache (47% of the eval's anchors were stale this way). Root cause: the worker presigned-PUT a `.drv` NAR, and the hourly orphan-files pass (`cleanup_orphaned_cache_files`) listed it on disk *before* the eval committed the `derivation`/`cached_path` rows — so the keep-set didn't reference it yet — and reclaimed it. The later `NarUploaded` then created a zombie `cached_path` (row present, object gone) that CacheQuery reported as cached, so `push_drv_closure` skipped re-pushing and dependents failed `InputsUnavailable`. Fix: the pass now skips any NAR younger than the orphan grace (`keep_orphan_derivations_hours`, default 24h, `<= 0` disables it), via the new `NarStore::list_hashes_with_modified`. `backend/gradient-cache/src/cacher/cleanup.rs`: `fresh_orphan_nar_spared_within_grace` writes an orphan NAR with the 24h grace active and asserts it survives; `keeps_active_drops_orphan` / `drops_everything_when_no_keep` disable the grace (`make_state` sets it to 0) so their immediate-reclamation assertions still hold.

## Unbacked-output sweep keys on the backing NAR, not is_cached/closure_complete

An eval (`019f15a5`) sat `Waiting` with builds that should have been possible: 15 producers were `status=Completed`, `is_cached=true`, `external_url=NULL`, but had no backing NAR object (a GC'd `cached_path` or an old global cache hit marked Completed without ever building - 0 build attempts). Their `closure_complete` was (correctly) false, so dependents were blocked at promotion and never dispatched - so no build reported the path missing and the reactive `reconcile_missing_inputs` never fired. The proactive `demote_unbacked_trusted_outputs` sweep first gated on `db.closure_complete` (skipped them), then on `o.is_cached` - but eval `019f1a38` surfaced the complementary dead zone: `cuda12.9-libcurand` was `status=Completed` with its `out` output `is_cached=false`, no `cached_path`, no `external_url`, and 0 build attempts (a partial cache-hit marked the anchor done without ever caching every output), so an `is_cached`-gated predicate skipped it and the whole CUDA/ML subtree (`torch`, `transformers`, ...) stranded for over a day. `UNBACKED_TRUSTED_OUTPUTS_SELECT` now keys on the **ground truth** - any output of a `status IN (3,7)` anchor with no backing `cached_path` NAR and no `external_url` - independent of both derived flags, and runs on the dispatch tick too so a stuck eval heals without waiting for a new one. The completion path records each output's `cached_path` before flipping the anchor terminal (#303/#399), so a genuinely-complete anchor is never demoted mid-completion. `backend/gradient-db/src/cache_storage.rs`: `unbacked_trusted_select_matches_the_gate` asserts the predicate requires a missing backing NAR, skips `external_url` outputs, and does NOT contain `o.is_cached` or `db.closure_complete`. The demote+rebuild convergence is E2E-CI-covered (MockDatabase can't run the recursive demote).

## closure_complete is bidirectional (no stale-true dispatch)

A direct dep of `etc.drv` (`019f1042`) sat at `closure_complete=t` while a tiny transitive member of its closure (`unit-nginx.service`) was uncached, because `closure_complete` was monotonic - once set it survived the member being evicted/rebuilt or a dependency edge recorded after the fact. The dispatch gate trusts `closure_complete` for direct deps, so `etc.drv` dispatched and the prefetch failed `InputsUnavailable` on the uncached transitive output (the recurring "simple text file" failures). `reconcile_closure_complete` is now bidirectional: a CLEAR fixpoint resets any anchor whose `CLOSURE_COMPLETE_GATE` no longer holds (output uncached, a dependency regressed or newly recorded-and-incomplete) before the existing SET fixpoint, and it runs on the 5s dispatch tick in addition to eval completion / graph-unstick. `backend/gradient-db/src/promotion.rs`. The recursive CLEAR/SET SQL can't run on MockDatabase (sea-orm), so convergence is covered by E2E CI; the gate predicate is shared verbatim with the SET path and `propagate_closure_complete`.

## Corrupt cached NAR self-heals instead of failing terminally

A `cached_path` whose stored object bytes don't match its recorded `nar_hash` (object and metadata written by different producers - e.g. a non-reproducible local build desynced from upstream-substitute metadata, observed on `ruby3.4-delayed_job` whose embedded `.git/index` ctimes differed from cache.nixos.org's) made every consumer fail `prefetch import failed`, classified `transient`, retrying against the poison forever. The worker's NAR integrity check (`NarImporter::verify_hash`/`verify_size`) now raises a typed `CorruptCachedNar(path)`; the executor classifies it as `InputsUnavailable` with that path so `reconcile_missing_inputs` demotes the corrupt object and rebuilds the producer with consistent metadata. `backend/gradient-worker/src/proto/nar_import.rs`: `corrupt_cached_nar_survives_context_wrapping` asserts the typed error is recoverable from the anyhow chain after the loop's `.context(...)` wrapping, which is what the executor's `chain().find_map(downcast_ref)` relies on. The end-to-end demote+rebuild is exercised by E2E CI.

## InputsUnavailable retries in-eval instead of failing permanently

A build that failed `InputsUnavailable` was marked `FailedPermanent`, so its retry was deferred to a *new* evaluation. Because the `derivation_build` anchor is global/build-once, that permanent verdict leaked onto sibling evaluations: eval `019f1747` inherited a victorialogs anchor that eval `019f15a5` had failed at `06:59:20` (its input `initrd-linux` was momentarily uncached - a dispatch race), even though the input was re-cached 14s later, so `019f1747` hung in `Building` with the build never retried. The fix routes `InputsUnavailable` through the existing transient-retry gate: `decide_failure_outcome` now treats it like `Transient` (retry to the attempt budget), the self-heal resets the missing input's producer to `Created`, and the build is marked `FailedTransient`; the backoff re-queue plus `dispatch_ready_builds`' dependency re-check hold it in `Queued` until the input is rebuilt, then it dispatches and succeeds in-eval. The self-heal circuit breaker still forces `Permanent` once `inputs_unavailable_max_loops` trips (unrecoverable input). `backend/gradient-scheduler/src/build.rs`: `inputs_unavailable_retries_like_transient_then_permanent` asserts the gate returns `Retry` while the budget remains and `Permanent` when spent; `inputs_unavailable_circuit_opens_after_max_loops` (unchanged) covers the breaker. The in-eval re-queue + gate-hold sequence is exercised by E2E CI.

## Dependency-failure dead zone reconciled proactively

The reactive `cascade_dependency_failed` fails an anchor's dependents only on a fresh terminal-failure *transition*, so it cannot reach a dependent that becomes non-terminal **after** its dependency already failed: `requeue_failed_anchors` / `requeue_failed_closure_for_eval` thaw a dependent back to `Created` without re-checking its still-failed dependency, and a concurrent evaluation can re-fail a dependency after the dependent was thawed. Eval `019f1a38` hung in `Building` with ~35 anchors (`activate`, `man-paths`, `system-path-dbus`, `unit-fast-nix-*`, `nixos-system-*`) stuck `Created` behind already-`FailedPermanent`/`DependencyFailed` dependencies (`etc`, `system-path`, `fast-nix-gc`): no transition to cascade, the dispatch gate holds them (dependency not terminal-success), so `check_evaluation_done` never finalizes. `reconcile_dependency_failed` is the timer-tick, failure-side counterpart of the `promote_ready` backstop: a single recursive statement walks `derivation_dependency` upward from every terminal-failed anchor (`FailedPermanent=4`/`DependencyFailed=6`/`FailedTimeout=9`, mirroring the reactive cascade's set, excluding `Aborted=5`) and marks each reachable non-terminal anchor (`Created=0`/`Queued=1`/`FailedTransient=8`) `DependencyFailed`; the dispatch loop then finalizes the evaluations it settled (the bulk UPDATE bypasses the single-row status hook). `backend/gradient-db/src/promotion.rs`: `dependency_failed_reconcile_sql_mirrors_the_cascade` pins the SQL shape - seed set, target status, non-terminal-only UPDATE, and upward edge walk (no live DB in unit tests). The recursive convergence and per-tick finalization are covered by E2E CI.

## Frontend - workers page no-cache banner

When the active organization has no subscribed cache, the workers page shows
a banner instructing the admin to subscribe to a cache before workers can run.

- `WorkersComponent - no-cache banner` - banner show/hide specs at
  `frontend/src/app/features/organizations/workers/workers.component.spec.ts`

## Frontend - HTTP upstream Gradient-cache probe (#363)

The "Add HTTP Upstream" dialog probes the entered substituter URL for
`gradient-cache-info` to offer switching to the native Gradient protocol. The
probe must only fire for real absolute URLs and only trust a genuine
gradient-cache-info body, so a scheme-less input (which resolves to the SPA's
own origin and returns a 200 `index.html`) no longer reports a Gradient cache.

- `normalizeProbeUrl` / `isGradientCacheInfo` - pure-logic specs at
  `frontend/src/app/features/caches/cache-upstreams/cache-upstream-probe.spec.ts`
  (absolute http(s)-only URL gating; body must carry `GradientVersion` +
  `GradientUrl`).
- `CacheUpstreamsComponent - HTTP upstream probe` - wiring specs at
  `frontend/src/app/features/caches/cache-upstreams/cache-upstreams.component.spec.ts`
  (skips fetch for scheme-less input; suggests proto only on a valid body).

## Frontend - evaluation builds search reveal

The sidebar "Search builds" bar on the evaluation-log page is hidden by default
and revealed with `/` (when not already typing) or Ctrl/Cmd+F while the sidebar
holds focus; Escape closes it and resets the filter.

- `isTypingTarget` - pure guard spec at
  `frontend/src/app/features/evaluations/evaluation-log/keyboard.spec.ts`
  (true for input/textarea/select/contenteditable so `/` is not hijacked while typing).
- `EvaluationLogComponent - sidebar search visibility` - toggle specs at
  `frontend/src/app/features/evaluations/evaluation-log/evaluation-log.component.spec.ts`
  (open on `/` and focused Ctrl+F; closed otherwise; Escape resets).

## Frontend - component style budget (#325)

The `anyComponentStyle` budget in `frontend/angular.json` (`maximumWarning: 6kB`,
`maximumError: 10kB`) is the regression guard for per-component CSS bloat:
`ng build --configuration production` fails if any single component stylesheet
compiles over 10 kB. Large stylesheets are split into cohesive region files via
`styleUrls` (e.g. `evaluation-log.{component,sidebar,messages,log}.scss`) so each
stays under the warning threshold.

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
`trigger_evaluation` unit tests already present in `backend/gradient-ci/src/trigger.rs`:

- *already_in_progress*: project has an in-progress eval → item appears in `skipped` with `reason="already_in_progress"`.
- *no_previous_evaluation*: `trigger_restart_builds` finds no previous eval → `reason="no_previous_evaluation"`.
- *db_error during trigger*: DB returns an error inside the per-project loop → `reason="db_error"`.

These can be added as further `forge_hooks.rs` tests by extending the
`MockDatabase` chain to return an in-progress evaluation row (or error) instead
of the empty list at the in-progress-eval query position.

## Native PR-review approval unpark (#369)

A maintainer's approving PR review releases an approval-gated fork-PR run, the
same as `/gradient approve` or the GitHub "Approve and Run" check action.
`ParsedPullRequestReviewEvent::{from_github,from_gitea}`
(`backend/gradient-forge/src/webhook.rs`) normalises the forge payloads;
`handle_pull_request_review`
(`backend/gradient-web/src/endpoints/forge_hooks/trigger.rs`) verifies the
reviewer is a repo writer before unparking. GitLab is a no-op (no webhook on
merge-request approval).

Run with: `cargo test -p gradient-forge --lib webhook`

| Test name | Scenario |
|-----------|----------|
| `github_review_approved_by_maintainer` | GitHub `pull_request_review` `submitted`/`approved` → `approved=true`, reviewer/PR/repo extracted. |
| `github_review_changes_requested_is_not_approved` | `changes_requested` review → `approved=false`. |
| `github_review_dismissed_is_not_approved` | `dismissed` action even with `approved` state → `approved=false` (only fresh approvals count). |
| `gitea_review_approved_by_maintainer` | Gitea `reviewed` + `review.type=pull_request_review_approved` → `approved=true`. |
| `gitea_review_rejected_is_not_approved` | Gitea `pull_request_review_rejected` → `approved=false`. |

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

## GitHub App setup cleanup (#457)

Frontend (`pnpm --dir frontend exec ng test --include='**/health.component.spec.ts' --watch=false`):
- `hides the "Set up GitHub App" link` - the System Health admin action is omitted once the App is configured.

Frontend (`pnpm --dir frontend exec ng test --include='**/github-app.component.spec.ts' --watch=false`):
- `allows leaving the setup view without prompting` - `canDeactivate()` returns true when no credentials are shown.
- `prompts and blocks leaving when the user cancels` - `canDeactivate()` confirms and returns false while one-shot credentials are visible.
- `prompts and allows leaving when the user confirms`.
- `flags beforeunload while credentials are shown` - the `beforeunload` handler cancels the event so the browser warns before unload.

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

## Substitute relay - direct fallback for local-disk caches

A substitutable build (`external_cached`) relays its outputs into our cache by
downloading each NAR from upstream and pushing it back. The push asked the
server for a presigned PUT URL (`CacheQuery {Push}`) and hard-failed `no
presigned PUT url for <path>` when none came back - but `presigned_put_url`
returns `None` by design for local-disk stores (they accept direct `NarPush`
frames), so on a non-S3 cache every substitutable output (e.g. a `-source`
patch like `CVE-2023-39810.patch`) broke. The relay now mirrors `upload_one_nar`:
it branches on `push.url`, using a presigned PUT when present and the new
`nar::push_compressed_direct` (streaming the already-compressed bytes over the
WebSocket, same resume handshake as `push_direct`) when absent. Shared
`nar::compress_nar` produces the compressed bytes + metadata for either
transport, replacing `upload_presigned_bytes`.

Backend (`cargo test -p worker --bins proto::nar::tests`):
- `push_compressed_direct_streams_bytes_and_confirms` - an in-memory compressed
  NAR with no presigned URL reaches the server as `NarPush` chunks that
  reconstruct the bytes verbatim, followed by a `NarUploaded` carrying the
  supplied compressed/uncompressed metadata and references.

## Resumable NAR transfers (#225)

Interrupted NAR transfers resume from a byte offset instead of restarting
from 0, in both directions. Resume rests on byte offsets, the existing
`NarUploaded.file_size` length check, and a `stream_token` guard (no content
hashing). Receivers stage chunks to `*.partial` files; senders stay stateless
and seek to the requested offset. `PartialStore` staging is synchronous
`std::fs`, so both the server (push) and worker (pull) receivers run every disk
op on `spawn_blocking` - otherwise the blocking I/O stalls the shared runtime and
unrelated tasks (HTTP handlers on the server) hang during a transfer burst.
End-to-end resume across a real reconnect is exercised by the NixOS VM suite in CI.

Storage (`cargo test -p gradient-storage`):
- `partial::tests` - `append_then_resume_reports_len`, `non_contiguous_append_errors`,
  `token_mismatch_truncates_to_zero`, `discard_is_idempotent`,
  `namespaced_key_creates_subdir`, `total_bytes_sums_partials`, `gc_zero_ttl_disabled`
  cover the on-disk `PartialStore` (contiguous append, offset-0 restart, token guard,
  GC).
- `nar::tests::get_stream_from_*` - `NarStore::get_stream_from` returns the suffix from
  an offset, equals the full object at 0, and yields an empty stream past the end.
- `log::shard_tests::log_lives_in_two_char_shard_subfolder` - `FileLogStorage` writes
  to `logs/<last-2-hex>/<uuid>.log` (e.g. `…8814fe` → `logs/fe/`) and `read`/`list_logs`
  resolve through the shard.
- `log::shard_tests::startup_migration_relocates_flat_entries` - constructing
  `FileLogStorage` relocates pre-sharding flat `<uuid>.log` files and `<uuid>` chunk
  dirs into their shard subfolder.

Proto (`cargo test -p gradient-proto`):
- `tests::nar_stream_header_client_roundtrip` / `nar_request_resume_roundtrip` /
  `nar_stream_header_server_roundtrip` / `nar_push_resume_roundtrip` - rkyv
  round-trip of the four additive resume messages.
- `handler::socket::serve_nar_tests::serve_streams_full_payload_in_chunks` - the
  server now emits a leading `NarStreamHeader` before the `NarPush` chunks.
- `handler::dispatch::nar_receive_store_tests` - the disk-backed push receiver
  (now `async`, offloading disk ops to `spawn_blocking`): contiguous append,
  non-contiguous/overflow poisoning, cross-key budget, `note_header` resumable
  prefix + token mismatch, presigned-mode detection, poison-clear retry, and
  `finish` discard.

Worker (`cargo test -p gradient-worker`):
- `proto::nar_recv::tests::partial_store_resumes_across_reconnect` - a fresh
  receiver over the same partial root resumes a download from the staged prefix.
- `proto::nar_recv::tests::push_resume_gate_*` - the push-resume gate resolves
  with the server offset and defaults to 0 when the server never answers.
- `proto::nar::tests::trim_for_resume_skips_trims_and_passes` - the push-sender
  skips/trims regenerated compressed parts to resume from an offset.

## Fleet eval-cache pull/push handlers (#386)

The server serves a flake's eval-cache SQLite blob by `fingerprint`, mirroring
the NAR transfer: a pull returns `Miss`, a presigned-S3 GET URL, or an inline
`EvalCacheChunk` stream; a push grants `Skip` (size-guarded so a stale-small
blob never clobbers a larger one), a presigned PUT, or an inline upload. Blobs
live under `eval-cache/<fingerprint>` in object storage; an `eval_cache_store`
row indexes them and is upserted on the unique `fingerprint` index. Every
handler is best-effort: any error logs and sends the safe negative response.

Proto (`cargo test -p gradient-proto`):
- `handler::eval_cache::tests` - pure decision helpers and inline staging:
  `should_accept_push` size-guard (accept when no row / strictly larger, skip
  when equal-or-smaller), `pull_outcome` selection (Miss / Presigned / Inline),
  `push_mode` selection (Skip / Presigned / Inline), `storage_key` namespacing,
  deterministic `stream_token`, and the in-memory `EvalCacheReceiveStore`
  (contiguous append + finish, non-contiguous reject, over-budget reject,
  chunk-needs-open-stream, single-active fingerprint resolution, token roundtrip).

NixOS VM (`nix/tests/gradient/cache`):
- Phase 5b asserts the worker's on-disk eval cache
  (`/var/lib/gradient-worker/eval-cache/eval-cache-v6/*.sqlite`) grew past the
  4096-byte empty SQLite header after an evaluation. Regression guard for the
  cache being read/pushed while the eval worker is still alive, before nix
  commits the `AttrDb` transaction, which left every pushed blob empty and every
  flake re-evaluating cold.

## Eval-cache eviction sweep (#386)

A periodic server-side sweep bounds `eval_cache_store` by age and total size:
rows older than `GRADIENT_EVAL_CACHE_MAX_AGE_DAYS` are evicted first, then -
oldest-`updated_at` first - enough additional rows to bring the surviving total
`size_bytes` at or under `GRADIENT_EVAL_CACHE_MAX_TOTAL_BYTES`. The pure
`select_evictions` selector is unit-tested with fixed `NaiveDateTime`s (no wall
clock); the DB/storage loop mirrors the proven sign-sweep and is covered by E2E.

Cache (`cargo test -p gradient-cache`):
- `cacher::eval_cache_sweep::tests` - `select_evictions`: empty input → empty;
  under-budget + all fresh → empty; an over-age row evicted regardless of size;
  over-cap evicts oldest-`updated_at` until under cap; age + size combine
  without double-counting an id.

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
scheduler call `gradient_util::nix_hash::normalize_nar_hash` before
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

Backend (`cargo test -p gradient-core --lib upstream::tests`): the narinfo
lookup/parse is shared by the cache-query handler (worker pulls) and the
eval-time substitutability probe (scheduler), so it lives in `gradient-core`.
- `parse_upstream_narinfo_full_fields` - verifies the parser reads
  `NarHash`, `NarSize`, `FileSize`, `References`, `Deriver`, and `Sig` from an
  upstream `.narinfo` body so the worker receives enough metadata to build a
  `ValidPathInfo` and call `add_to_store_nar`. Without this the worker
  silently failed imports and the build died with
  "dependency does not exist, and substitution is disabled".
- `parse_upstream_narinfo_ca_field` - parses the `CA:` field so content-addressed
  (fixed-output) paths import correctly.
- `parse_upstream_narinfo_requires_url` - a narinfo without `URL:` is rejected.
- `parse_upstream_narinfo_trims_base_url_trailing_slash` - joins
  `base_url` + `URL:` without double slashes.
- `parse_upstream_narinfo_empty_references_is_some_empty` - `References:` with
  no paths yields `Some(vec![])`, not `None`.
- `parse_upstream_narinfo_ignores_unparseable_sizes` - malformed `NarSize` /
  `FileSize` fall back to `None` rather than aborting the parse.

`compute_upstream_substitutable` flags an anchor `substitutable` only when every
output is available on an **upstream** cache (`external_url`, set from a narinfo
probe hit - upstream serves the whole closure). Internal cache presence
(`is_cached`) is deliberately excluded: an output sitting in our store with an
**incomplete** runtime closure (a dependency failed or was purged) would
otherwise be flagged substitutable, fail substitution, and escalate into a build
whose inputs were never produced - the `activate`/`unit-bird.service`
`InputsUnavailable` loop. The genuinely-whole internal case is handled earlier by
`is_truly_substituted` (gated on `closure_complete`, which resigns rather than
dispatches). Exercised end-to-end by the cache integration test (the narinfo
probe + DB writes have no MockDatabase harness); `derivations_all_outputs_available`
covers the all-outputs-available predicate in isolation.

## Re-offering re-queued jobs

Backend (`cargo test -p gradient-scheduler --lib worker_pool`):
- `remove_sent_candidate_allows_reoffer` - a re-queued build loses its
  sent-candidate flag so the next delta push re-offers it (other jobs stay sent).
  Backs the dispatch self-heal: re-queued/rejected jobs get scored a second time
  and dispatch into free capacity instead of sitting unassigned. The worker-side
  cache drop on reject and the loop's per-pass re-offer are covered end-to-end in
  CI.

## Upstream substitutability (eval-time lookup)

Backend (`cargo test -p gradient-scheduler --lib upstream_substitutable_tests`):
- `substitutable_only_when_all_outputs_available` - `derivations_all_outputs_available`
  marks a derivation substitutable only when *every* one of its outputs is cached
  somewhere (gradient cache or an upstream); a derivation with any output missing
  is built. This is the all-or-nothing rule behind `compute_upstream_substitutable`.
- `no_outputs_is_not_substitutable` - a derivation with no recorded outputs is
  never substitutable.

The full probe (`compute_upstream_substitutable`: org-scoped `.narinfo` lookup,
persisting `derivation_output.external_url` + metadata, and `query()` serving the
persisted URL without re-running narinfo) is covered end-to-end in CI - there is
no real-Postgres/HTTP unit harness for it.

## Eval BFS pruning honours substitutable closures

Backend (`cargo test -p gradient-proto --lib prunable_known_derivations_tests`):
- `prunes_only_outputs_on_a_real_upstream` - the `QueryKnownDerivations` handler
  may prune a derivation's subtree only when **every** output is on a real upstream
  cache (`external_url`). An upstream binary cache serves a *complete closure*, so
  the pruned subtree's outputs are fetchable on demand. Our own cache (`is_cached`
  / `cached_path`) is **not** accepted: it is output-only (substitution relays just
  the output NAR; a config-specific node's subtree may never have been pushed), so
  pruning on it strands that subtree - never walked, recorded, or built, and
  off-upstream so unfetchable, a permanent `InputsUnavailable` dead-end (observed
  live: `unit-*.service` -> `X-Restart-Triggers-*` / `unit-script-*`, none on
  cache.nixos.org, 0 rows in the DB). The cost - re-walking our own (unreliable)
  cached closures every eval - is the correctness price of an output-only cache;
  upstream-served nixpkgs stays pruned via persisted `external_url`. A derivation
  with any output not on an upstream, no outputs, or no rows is not pruned.

## Promotion gated on `edges_complete`

`derivation_build.edges_complete` gates `promote_ready`, `promote_dependents`, and
the `dispatch_ready_builds` readiness query. Anchors are created per-batch while an
evaluation streams, but `derivation_dependency` edges are flushed in one pass at
its completion - so an anchor left edge-less by a failed/aborted/restart-interrupted
or still-running eval would otherwise look dependency-free and be dispatched without
its inputs (`InputsUnavailable`, observed as `recorded_edges = 0` on a build that
still needs inputs). `handle_eval_job_completed` calls `mark_edges_complete_for_eval`
right after the flush; the flag is monotonic (edges are content-addressed, never
rewritten), so a later `requeue_failed_anchors` keeps the anchor promotable without
re-evaluation. It marks the eval's **full dependency closure**, not just its
directly-reported `build_job`s: a transitive dep reached only via global edges
(pruned or substituted in this eval, so no `build_job` here) would otherwise never
get its flag maintained, and a prior demote that cleared it leaves the dep
`edges_complete = false` forever - unpromotable behind the gate even though its edge
set is complete and satisfied (observed live: `tzdata-2026b`, `edges_complete=f`, 0
unmet deps, 30 build_jobs, blocking the `etc`->`activate`->`nixos-system` chain). A
closure node is marked when it has recorded build edges or is one of this eval's own
0-dep `build_job` leaves; ambiguous 0-edge transitive nodes stay gated. The
graph-stuck heal (`attempt_graph_unstick`) re-runs it so an already-parked eval
recovers without a re-trigger.
The migration backfills existing rows complete unless they are `Created`, never
dispatched, and have zero edges - the exact shape of an anchor stranded by an
incomplete eval. The promotion/dispatch statements are raw SQL (no MockDatabase
harness, per the backend test notes); covered end-to-end in CI.

## Startup recovery aborts interrupted evals' anchors

Backend (`cargo test -p gradient-db --lib recovery`):
- `all_operations_populate_report` - `recover_interrupted_work` reports all five
  recovery actions, including the new `builds_aborted`: the anchors driven by the
  pre-build evals it aborts are themselves aborted (`abort_anchors_for_evals`),
  mirroring the explicit-abort path so the server matches the builder, which
  aborts the eval's builds when the server dies. The forced re-evaluation
  re-drives them (`requeue_failed_anchors` resets `Aborted -> Created`).
- `project_force_step_skipped_when_no_pre_build_evals` - with no in-flight
  pre-build evals, the abort-anchors, abort-evals, and force-eval steps are all
  skipped and `builds_aborted` stays 0. The shared-anchor exclusion (an anchor a
  still-live eval also needs is left running) is raw SQL, covered E2E in CI.

## External-cached substitution is a pure NAR relay (no store, no closure)

A substitute build (`external_cached`) is done by `relay_external_cached_outputs`:
for each output it `CacheQuery Pull`s the upstream narinfo (URL, `nar_hash`,
`references`), downloads the **one** output NAR from upstream, decompresses +
verifies it, recompresses to zstd (`nar::upload_presigned_bytes`), and pushes it
straight into our cache via a presigned PUT - **without** `add_to_store_nar` or
fetching the runtime closure. The previous path imported the output **plus its
whole runtime closure** into the local nix store (`add_to_store_nar` requires every
reference to be a valid store path), so any gap in the closure failed the import,
surfaced as `SubstituteUnavailable`, and escalated into a real build that then died
on `InputsUnavailable`. The closure no longer matters for the substitute itself:
each closure member is mirrored by its own anchor, and the `closure_complete` gate
orders dependents. A relayed output is therefore in our cache but not
`closure_complete` until its closure is also mirrored - exactly what the gate
expects. Covered end-to-end in CI (the relay needs a real upstream + object store,
so there is no local unit harness); `nar::upload_presigned`'s helpers keep their
existing tests.

## Cache GC keeps the dispatch-gate trust invariant

The cache GC deletes `cached_path` rows whose NAR object is gone (zombie purge) or
expired (TTL eviction) without clearing the producing anchor's flags, so a producer
could sit at `Completed`/`Substituted` + `closure_complete` with no fetchable
output - the dispatch gate trusts it and every dependent fails `InputsUnavailable`
permanently, and being terminal-success it is never re-queued.
`demote_unbacked_trusted_outputs` (gradient-db) selects every terminal-success
producer with any output that is neither in our cache nor on an upstream and demotes
it to `Created`; it runs hourly in the cache loop, at eval-resolve, and on the
dispatch tick. The behaviour needs a real Postgres (no local harness), so the SQL is
pinned by a shape test:

- `cache_storage::tests::unbacked_trusted_select_matches_the_gate` - asserts the
  reconciler's select targets terminal-success anchors (`status IN (3, 7)`), skips
  upstream-served outputs (`external_url IS NULL`), and requires a missing backing
  NAR (`NOT EXISTS … cp.file_hash IS NOT NULL`) - keyed on that ground truth, not on
  `o.is_cached` or `db.closure_complete` (both false for the dead-zone anchors), so
  it rescues a never-cached output yet never demotes a still-fetchable or
  upstream-only producer.

## Derivation GC is a mark-and-sweep over the live closure

`build_job` rows are per-eval and pruned with old evals, but the global
`derivation_dependency` graph and the build-once anchors persist. The old orphan
pass deleted any derivation with no `build_job`, sweeping away build inputs of
retained closures and stranding dependents on `InputsUnavailable`.
`gc_orphan_derivations` now reclaims a derivation only when it lies outside the
build-dependency closure of the live roots (`entry_point` ∪ `build_job`), and
`active_hashes` additionally pins input-source and `.drv` hashes of live
derivations so a concurrent GC cannot delete a freshly-pushed source/`.drv`
mid-build. The graph traversal needs a real Postgres, so the keep-set is pinned by
a shape test:

- `gc::tests::reachable_cte_closes_over_roots_and_dependency_edges` - asserts the
  keep-set seeds from `entry_point` and `build_job` and recurses
  `derivation_dependency` toward each root's dependencies, so a transitive build
  input whose own `build_job` was pruned still survives GC.

Evaluation GC deletes old evals and relies on FK cascade for their per-eval rows
(`evaluation -> build_job -> build_attempt -> build_log_chunk`). `build_log_chunk`
held a bare `build_attempt` UUID with no FK, leaking its chunk-index rows once the
eval was collected; migration `m20260626_000001_build_log_chunk_cascade` purges the
existing orphans and adds the missing `ON DELETE CASCADE`. The cascade needs a real
Postgres, so it is covered by the migration apply + the per-project eval-GC E2E path
rather than a `MockDatabase` test.

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
- `nix::store::tests::pool_config_connection_timeout_is_generous` - regression
  for `acquire daemon connection: timeout: connecting to daemon`. The worker's
  pool config must override harmonia's 10 s `connection_timeout` default, not
  just `acquire_timeout`. Under a high daemon connection count the handshake
  for a fresh pooled connection routinely outlasts 10 s, so the build failed an
  otherwise-healthy prefetch import; the config now grants connection
  establishment the same 10 min ceiling as the acquire wait.

## Missing-input self-heal (#410)

A build whose prefetch finds a required input absent from the cache no longer
dies as an opaque permanent failure that recurs every evaluation. The worker
classifies it as `BuildFailureKind::InputsUnavailable` and forwards the
offending paths on `JobFailed.missing_paths`. The server then purges each
path's stale cache artifact - deleting the `cached_path` row and clearing the
output's `is_cached` / `cached_path` while leaving the derivation graph intact -
so the next evaluation sees the output as never cached and rebuilds it from
scratch. The failing build stays terminal for this eval (`InputsUnavailable` is
permanent); the rebuild belongs to the next evaluation.

Backend (`cargo test -p gradient-worker -p gradient-scheduler --tests`):
- `proto::nar_import::tests::missing_inputs_message_and_downcast` - the typed
  `MissingInputs` error renders the human message (count + first path) and
  survives `anyhow` boxing, so the executor can `downcast_ref` it and forward
  the exact paths to the server.
- `build::retry_tests::inputs_unavailable_is_terminal_no_retry` -
  `decide_failure_outcome(InputsUnavailable, ..)` is always `Permanent`: the
  build fails this evaluation rather than burning the retry budget against an
  input that will not appear until its producer is rebuilt.
- `build::retry_tests::inputs_unavailable_circuit_opens_after_max_loops` -
  `inputs_unavailable_circuit_open(prior, max_loops)` self-heals for the first
  `max_loops` failures and opens afterward, so an unrecoverable input stops
  churning the cache and fails fast instead of looping forever.
- `build::retry_tests::truncate_failure_message_bounds_long_input_on_char_boundary` -
  the worker error persisted on `build_attempt.failure_message` is capped on a
  UTF-8 char boundary (full text still lands in the build log).
- `build::retry_tests::store_path_hash_extracts_32_char_hash` - the helper that
  maps a missing `/nix/store/<hash>-<name>` path to the `derivation_output`
  hash used to purge its cache artifact.
- `proto::nar_import::tests::presigned_404_410_are_missing_inputs_other_statuses_retry` -
  a presigned S3 download that 404/410s is reclassified as a missing input
  (the bucket lost an object the DB still claims), while 403/429/5xx stay
  retryable transport errors. The server never sees this since it only signs
  the URL, so the worker is the one that triggers the self-heal.
- `proto::nar_import::tests::presigned_retryable_statuses_are_timeout_rate_limit_and_5xx` -
  `presigned_status_is_retryable` returns true for 408/429/5xx and false for
  400/403/404/410. `download_by_url` now bounds presigned downloads to
  `PREFETCH_CONCURRENCY` and retries each one (`download_one_presigned`,
  `PRESIGNED_DOWNLOAD_MAX_ATTEMPTS` with exponential backoff) on transport
  errors and retryable statuses, so a flaky object store (`tls handshake eof`
  under unbounded concurrent connections) no longer fails the whole build's
  prefetch. 404/410 still surface immediately as `MissingInputs`.

The purge path itself (`gradient_db::demote_cached_output`) deletes the
`cached_path` row (its `cached_path_signature` rows cascade; the
`derivation_output` FK is `ON DELETE SET NULL`), deletes the NAR object, and is
shared with the NAR-serve self-heal in `socket.rs`. It also **resets the
producing anchor** from terminal-success (`Completed`/`Substituted`) back to
`Created` (`substitutable`/`substituted` cleared, `attempt = 0`): the artifact is
gone, so the build-once "succeeded" invariant no longer holds, and `resolve_anchors`
never re-queues terminal-success anchors. Without the reset a producer whose NAR
was demoted/zombie-purged stays "succeeded" forever and every dependent fails
`InputsUnavailable` indefinitely (e.g. `search-meta` missing a `nixpkgs--.json`
input). `demote_deletes_the_nar_object` covers the `.drv` (no-producer) path; the
anchor reset is raw SQL like `requeue_failed_anchors` and is exercised end-to-end
in CI (the db crate has no real-Postgres unit harness).

The cache is closure-complete by invariant: `compress_and_push_paths` pushes each
output's full runtime closure (`collect_runtime_closure`), not just the output, so
a referenced source (`-source`) can never be stranded uncached while its referrer
is cached. The invariant is **enforced at dispatch** via `derivation_build.closure_complete`:
a **built** anchor is complete once its outputs are cached, its edges are flushed,
and every build dependency is itself `closure_complete` **or** `substitutable`
(closure fetchable from upstream). A build's runtime refs are a subset of its build
inputs, so a fetchable build closure implies a fetchable runtime closure - closing
the runtime-vs-build-time edge gap (`unit-bird.service` via `system-units`) without
a runtime walk. `propagate_closure_complete` (from `update_derivation_build_status`
at terminal success, **before** promoting) computes it over `derivation_dependency`
and **ripples up**: it marks the just-finished anchor, then re-checks that anchor's
dependents, etc. The up-ripple is load-bearing - a dependent that finished before
its dependency did would otherwise never re-evaluate its completeness, and earlier
the VM test stalled `Building -> Waiting` for exactly this. A **substituted** anchor
is deliberately not marked complete (we hold only its output NAR, not its build
closure); dependents reach it through `substitutable`. `promote_ready` /
`promote_dependents` / `dispatch_ready_builds` gate each dependency on
`(terminal-success AND closure_complete)` **or** `substitutable`.

`propagate_closure_complete` only fires on a fresh completion event, so anchors
that completed under older code never get the build-edge flag and would strand
their dependents in `Created` with no error to trigger a reactive heal.
`reconcile_closure_complete` (global fixpoint, run at eval completion before
`promote_ready`) closes that gap: it re-applies the same gate to every unflagged
built anchor, converging bottom-up in O(longest unmarked chain); a converged
graph costs one zero-row statement. The same fixpoint unsticks a live instance
manually via a single `DO` loop over `derivation_build`.

Out-of-order substitution (#456): a `substitutable` anchor (NAR on an upstream
cache) skips the dependency gate entirely in all three of `promote_ready` /
`promote_dependents` / `dispatch_ready_builds` - it needs neither its build
dependencies nor its runtime closure in our cache, since the worker fetches the
output plus closure from upstream on demand (never the `.drv`'s build-time
`input_sources`). The substitute job carries empty `required_paths`, so the worker
pulls no build deps and every worker scores it a uniform zero. The gate rewrites
are exercised end-to-end in CI (no real-Postgres unit harness); `decide_dispatch_
mode` unit tests still cover the substitute/escalate/stall decision.

Self-heal clears the flag so the gate re-blocks: a reported-missing leaf with a
producer is purged + rebuilt and `clear_closure_complete_for_referrers` drops
`closure_complete` up the (transitive) referrer chain without deleting healthy
NARs; a producerless source demotes its direct referrers (`demote_referrers_of`)
so a referrer rebuild re-pushes it. A third case is the **orphan producer**: the
missing leaf has a producing derivation, but the eval pruned it out of the build
graph (a referrer's output was cached without its closure under output-only
substitution), so it carries no `build_job` and promotion can never queue it - the
gentle flag clear leaves the referrer cached, pruned, and never re-walked.
`reconcile_missing_inputs` detects this (`derivation_is_reachable` false for the
demoted producer) and demotes the referrers so the next eval re-walks them,
re-records the dropped `derivation_dependency` edge, and schedules the orphan.
`demote_cached_output` leaves `edges_complete` intact - it deletes the
`cached_path`, so the uncached output is re-walked by the next eval regardless
(uncached nodes are never pruned), and clearing the flag would only strand a
complete-edge node (e.g. a shared dep `tzdata` swept up by the absent-orphan
recovery) behind the closure gate until that re-walk, which manifests as a
`graph_stuck` deadlock. Reproduced live: a
crane `vendor-cargo-deps` was `Completed`/`closure_complete` with only 2 of its
edges recorded, its `vendor-registry` dependency a 0-edge/0-`build_job` orphan, so
`gradient-server-deps` dispatched and the prefetch closure-walk died
`InputsUnavailable` on the orphan's output.

The fourth and self-healing case is the **absent orphan**: the missing input has
no producer row *and* no indexed referrer (pruned out so thoroughly it was never
recorded, or its rows were deleted by an admin), so it cannot be reached upward -
`demote_referrers_of` finds nothing. `reconcile_missing_inputs` flags this
(`needs_dep_rewalk`) and reaches it downward from the known failing build:
`demote_output_only_cached_deps` demotes that build's output-only-cached direct
dependencies (`cached_path.file_hash` present, `external_url` NULL), so the next
eval re-walks them and re-records the orphan plus its now-buildable subtree.
Upstream deps (`external_url`) are left intact - a real upstream serves their
closure. This is what lets an accidental cache-row deletion recover on the next
evaluation instead of needing a manual reset; reproduced live with
`cargo-package-lzma-sys-0.1.20`, which had no `derivation`/`derivation_output`/
`cached_path_reference` rows at all. Exercised end-to-end in CI (the db crate has
no real-Postgres unit harness). References are fully normalized into `cached_path_reference` (one row per referrer
-> referenced store path, with `reference_hash` for indexed lookup and `position`
for stored order); the `cached_path.references` text column is dropped. Referrer
lookups (`referrers_of_hash`) and the runtime-closure walks become exact index
scans instead of a `references LIKE '%hash%'` full-table scan, and the narinfo
`References:` line plus the signature fingerprint reconstruct verbatim via
`references_for_hash` (`ORDER BY position`) - the worker sends references in nix
`StorePathSet` order, so `position` preserves the exact bytes the signature
covers. The migration backfills the flag to a fixpoint over the existing cache and
resets closure-incomplete terminal anchors so they rebuild. The closure marker, gate, reference index, and
migration backfill are exercised end-to-end in CI (the db crate has no
real-Postgres unit harness).

Preventively, `expand_substituted_closure` now only marks a closure dep
`Substituted` when its `derivation_output` rows are all `is_cached = true`;
otherwise it inserts a `Created + substitutable = true` build so the dep
substitute-attempts and escalates to a real build on a miss. The `cached` flag
is computed in the recursive-CTE query and is covered by E2E CI (the scheduler
has no real-Postgres unit harness for raw SQL).

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
  (`backend/gradient-types/src/cli/storage.rs`) - `StorageArgs::default()`
  yields `keep_evaluations = 30` so a default `gradient-server` install
  bounds per-project evaluation retention instead of allowing unbounded
  growth (issue #92).
- `default_nar_ttl_hours_is_two_weeks`
  (`backend/gradient-types/src/cli/storage.rs`) - `StorageArgs::default()`
  yields `nar_ttl_hours = 336` (2 weeks) so cached NARs eventually
  expire on a default deploy (issue #92).
- `clap_default_keep_evaluations_is_thirty`
  (`backend/gradient-types/src/cli/storage.rs`) - parsing an empty argv
  through clap yields `keep_evaluations = 30`, guarding against drift
  between the `#[arg(default_value …)]` attribute and the `Default`
  impl.
- `clap_default_nar_ttl_hours_is_two_weeks`
  (`backend/gradient-types/src/cli/storage.rs`) - parsing an empty argv
  through clap yields `nar_ttl_hours = 336`, same drift guard.
- `state_project_silently_ignores_legacy_force_evaluation_field` -
  state files written before the rename may still carry
  `force_evaluation`; serde's default unknown-field handling drops it
  silently so existing deployments parse cleanly after the field's
  removal from the schema.
- `state_org_accepts_explicit_id` / `state_org_id_defaults_none` -
  `StateOrganization.id` deserialises from a UUID string and defaults to
  `None`, so a declarative deployment can pin the org UUID a worker's
  `peersFile` references (`<org_id>:<token>`) (issue #333).
- `state_org_validator_rejects_malformed_id` - a non-UUID `id` yields a
  validation error pinpointing `organizations.<org>.id`.
- `state_org_validator_rejects_duplicate_ids` - two organizations
  declaring the same `id` is an error.
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
  (`backend/gradient-state/src/provisioning.rs`) -
  `apply_pending_org_memberships` is a no-op when the username has no
  pending entries; callable from any user-creation path without a
  matching state declaration.
- `keep_set_tests::keep_sets_track_inner_name_not_attrset_key`
  (`backend/gradient-state/src/provisioning.rs`) - `gradient-state.nix`
  exposes `name = mkOption { default = <attrset key>; }` on users,
  organizations, projects, caches, and API keys, so a user may pin
  `projects.foo = { name = "main"; … }`. Every `apply_*` writes the
  override value to the DB row; `unmark_removed_entities` therefore must
  also key its keep-sets on the value's `name`/`username` field, not on
  the HashMap key, or the cleanup pass deletes (or unmarks) the row the
  same reconciliation just inserted.

These pin the wire contract between `nix/modules/gradient-state.nix`
(`types.nullOr types.str` on `password_file` and the three `description`
options) and `backend/gradient-state/src/mod.rs`. Without them, provisioning a
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

## Integrations apply before projects, validated at build time (#332)

Backend (`cargo test -p core --lib state::tests`):
- `state_reporter_trigger_accepts_declared_inbound_integration` - a
  `reporter_push` trigger naming a declared inbound integration in the same
  org passes validation.
- `state_reporter_trigger_rejects_unknown_integration` - a reporter trigger
  naming an integration that is not declared yields a
  `projects.<project>.triggers` validation error, surfacing the mistake at
  build/`--validate-state` time instead of mid state-apply.
- `state_reporter_trigger_rejects_outbound_integration` - a reporter trigger
  pointing at an `outbound`-kind integration is rejected (reporters resolve
  against inbound integrations only).
- `state_reporter_trigger_accepts_github_app_name` - the reserved `github`
  integration name needs no explicit declaration (auto-managed GitHub App row).

The apply order in `apply_state_to_database` now runs `apply_integrations`
before `apply_projects` so trigger/action integration lookups resolve against
rows already in the DB.

## `--validate-state` parses without secret files

Backend (`cargo test -p core --lib types::cli::secrets::tests`):
- `validate_state_parses_without_secret_files` - `--state-file ... --validate-state`
  parses with no `--crypt-secret-file`/`--jwt-secret-file`, so the secret-free Nix
  build/CI check (`services.gradient.validateState`) runs without provisioning secrets.
- `secret_files_parse_from_flags` - the live-server path still accepts both secret
  flags; `init_state` rejects an empty value before startup.

## Scheduler does not double-dispatch a build

Backend (`cargo test -p scheduler --lib jobs::tests::add_pending_does_not_requeue_active_job`):
- `add_pending_does_not_requeue_active_job` - once a build is assigned (moved to
  `active`), re-adding the same job id must not put it back in `pending`. Two
  concurrent `dispatch_ready_builds` passes can both clear the `contains_job`
  filter before either enqueues; without the idempotency guard the same build is
  dispatched to the worker twice, the duplicate is aborted by the nix daemon
  ("build aborted by server"), and the spurious failure fails the whole
  evaluation (observed flaking the `gradient-cache` NixOS VM test).

## Cross-architecture substitutable-build scheduling

Backend (`cargo test -p gradient-scheduler --lib dispatch_mode::tests`):
- `non_substitutable_is_real_arch` / `substitutable_under_threshold_is_builtin` / `escalates_only_when_arch_worker_present` / `stalls_when_budget_spent_and_no_arch_worker` / `arch_available_builtin_always_true` - verify `decide_dispatch_mode` and `arch_available` for the (substitutable, miss_count, threshold, arch_has_worker) combinations.

Two retry-scoping changes make a *new* evaluation a fresh build intent against
the global, build-once anchor (covered E2E in CI; the SQL is not MockDatabase-
testable):
- `substitute_miss_counts` (`gradient-db/src/build_attempt.rs`) is keyed by
  `(anchor, evaluation)` via the attempt's `build_job`, so a new eval starts the
  substitute-miss budget at zero instead of inheriting a previous eval's
  exhausted budget and escalating straight to a build. Dispatch and the parker's
  `BuildabilityChecker` both look up the count for their driving evaluation.
- `requeue_failed_anchors` (`gradient-db/src/promotion.rs`), called from
  `resolve_anchors`, resets anchors a previous eval left terminal-failed
  (`FailedPermanent`/`Aborted`/`DependencyFailed`/`FailedTimeout`) back to
  `Created` for the new eval's derivations, so a permanent failure is retried
  rather than poisoning every later eval that needs the derivation. Build-once
  success (`Completed`/`Substituted`) is never reset.

Backend (`cargo test -p gradient-db --lib build::tests`):
- `maps_returned_rows_to_id_set` / `empty_input_returns_empty_set` - plumbing tests for `builds_with_satisfied_deps` (the SQL antijoin itself is covered end-to-end in CI).

Backend (`cargo test -p gradient-scheduler --lib build::tests`):
- `stalled_substitute_is_not_buildable_and_appears_in_unmet` - when a build's substitutable derivation has too many misses, the parker does not mark it buildable and the reason reflects the escalation.
- `dependency_blocked_build_is_not_buildable` - a build waiting on an unsatisfied dependency is not buildable regardless of dispatch mode.
- `substitutable_within_budget_is_buildable_anywhere` - a substitutable build under the miss threshold is buildable on any worker with the derivation's architecture.

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

## Org members get read-only cache access (#334)

Backend (`cargo test -p web --lib access::tests`):
- `effective_cache_mask_returns_role_for_member` - a direct cache member's API
  key is capped by their cache role mask.
- `effective_cache_mask_returns_view_for_org_subscriber` - a member of a
  subscribed organization with no `cache_user` row is treated as read-only
  (`cache_view_mask`), so they can mint a read-only cache key and view stats.
- `effective_cache_mask_none_for_outsider` - a user with neither a cache role
  nor a subscribing org gets no mask, so cache-pinned key creation 404s.

These pin the fix for the remaining "cache not found" responses: creating a
read-only cache API key (`POST /user/keys`) and reading cache traffic/storage
metrics (`GET /caches/{cache}/stats`) now route through the same
member-or-subscriber visibility as `GET /caches/{cache}`, instead of requiring
`ManageCacheMembers` / cache ownership.

Integration (`cargo test -p web --test cache_api_key_pinning`):
- `create_cache_pinned_key_cannot_exceed_member_mask` - a View-mask member is
  denied a `writeStore` cache-pinned key (perms beyond their mask) → 403.

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

## Scheduler job-notify is level-triggered (#359)

Backend (`cargo test -p scheduler --tests scheduler_tests::job_notify_bump_is_not_lost_when_not_awaiting`):
- An `enqueue_*_job` that bumps `Scheduler::job_notify` while no session is
  awaiting `changed()` is still observed on the next check (`has_changed()` is
  `true`). Guards against the regression where an edge-triggered
  `Notify::notify_waiters()` dropped wakeups fired during NAR/job traffic,
  starving deep build chains of `JobOffer`s and timing out the cache VM test.
- `dispatch_kick_is_retained_when_not_awaiting` - a `kick_dispatch()` fired
  while the dispatch loop is mid-pass is retained (`notify_one` permit) and woke
  on the next iteration, so serial chains advance at completion speed rather than
  one level per 5s tick.
- `worker_pool::tests::test_assign_and_release_job` - `release_job` reports the
  worker idle only after its last in-flight job is released, so the dispatch kick
  fires for a now-idle worker (serial chain) but not while it is still building
  (e.g. 1 of 8 done).
- `worker_pool::tests::reregister_preserves_reported_capabilities` - a reconnect
  or server-initiated re-auth re-registers a worker, but architectures/features/
  sizing arrive once per session via a separate `WorkerCapabilities` message the
  worker need not re-send. `register` must carry the prior slot's reported
  capabilities over; otherwise the slot resets to empty architectures,
  `can_build` rejects every real-arch job, and builds queue forever against an
  idle worker (eval parked `Waiting` with a stale, empty-`unmet` reason).

## Push-mode signature placeholders - insert-select, FK-race-safe

`ensure_push_signatures` (`backend/gradient-proto/src/handler/cache.rs`) creates
one `cached_path_signature` placeholder per (cached_path, org cache) pair when a
worker connects in Push mode - a cartesian product over the worker's whole store.
It inserts via `INSERT ... SELECT FROM cached_path ... CROSS JOIN unnest(caches)`
keyed on `cp.id = ANY($paths)`, so a path concurrently purged (demote/GC) between
the worker's CacheQuery and this insert is simply skipped rather than violating
the `fk-cached_path_signature-cached_path` foreign key and failing the whole
batch. Array params keep each statement to two binds regardless of row count
(no 65 535-param cap concern); the path list is chunked at `SIGNATURE_PATH_BATCH
= 8000` only to bound statement size for large stores. Verified by E2E CI
(MockDatabase cannot exercise the FK race).

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

`evaluations_to_gc` (`backend/gradient-db/src/gc.rs`) decides, by index into a
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

## Orphan derivation GC - race-safe delete + NAR reclaim

`gc_orphan_derivations` (`backend/gradient-db/src/gc.rs`) reclaims global,
content-addressed `derivation` rows that no surviving evaluation needs. Because
a derivation is reused across evaluations, a concurrent eval can re-attach a
`build_job` to a past-grace orphan between selection and deletion, so the pass
deletes rows with an in-statement `NOT EXISTS (build_job)`/`NOT EXISTS
(entry_point)` re-check and `RETURNING`, then reclaims NARs keyed strictly to
the rows actually deleted. This fixes the `build_job_derivation_fkey` violation
seen after the globalize-derivation migration and the latent corruption where a
re-referenced derivation was left pointing at an already-deleted NAR.

- `gc::tests::reclaims_only_hashes_no_survivor_references` - a hash shared by a
  surviving `derivation_output` (e.g. a fetchurl source tarball) keeps its NAR;
  only the unshared hash is reclaimed.
- `gc::tests::reclaims_nothing_when_all_hashes_survive` - every deleted hash
  still referenced means no NAR is removed.
- `gc::tests::reclaims_all_when_no_survivors` - no surviving reference means all
  deleted hashes are reclaimed.

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
`backend/gradient-ci/src/reporting.rs` and are reused by the
`ForgeStatusReport` action dispatcher (`backend/gradient-ci/src/actions.rs`):

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
| `cache` | `/cache/{cache}/...` (public NAR surface) and `/cache/{cache}/proto` (WS upgrade) | 1 req / 20 ms | 3000 |
| `cache-inspect` | `/cache/{cache}/{ls,serve}/...` | 1 req / 333 ms | 180 |
| `cache-log` | `/cache/{cache}/log/{drv}` | 1 req / 333 ms | 900 |
| `default` | everything else under `/api/v1` and `/proto` | 1 req / 200 ms | 150 |

The limiter keys solely on client IP: authenticated and anonymous cache requests share the same per-IP tier. The cache proto WS upgrade is pinned to the NAR-download tier; its concurrent fan-out is separately capped by `GRADIENT_PROTO_ANON_MAX_CONNECTIONS_PER_IP`.

Client IP is extracted from `X-Forwarded-For` / `X-Real-IP` (deployments
are expected behind a reverse proxy), falling back to `ConnectInfo`,
falling back to a single global bucket if no signal is available
(prevents 500s in tests / direct hits).

Tests (`cargo test -p web --test rate_limit`):

- `auth_tier_throttles_burst` - 5 successive `POST /api/v1/auth/check-username`
  requests succeed, 6th returns `429`.
- `cache_tier_does_not_throttle_moderate_burst` - 50 successive GETs to
  `/cache/{cache}/nix-cache-info` never return `429`.
- `cache_proto_tier_does_not_throttle_burst` - 250 successive GETs to
  `/cache/{cache}/proto` never return `429`, proving the upgrade shares the
  generous NAR-download tier (burst 3000).

## Outgoing webhook URL - SSRF validation

`validate_webhook_url` (in `backend/gradient-util/src/http_validation.rs`) is the gate
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
`backend/gradient-forge/src/reporter.rs`) now validate any user-supplied
`base_url` / `api_base_url` through the same SSRF gate as outgoing
webhooks (`validate_webhook_url`), and build their reqwest clients with
`redirect::Policy::none()` so that an attacker cannot pivot a status
POST to an internal endpoint and leak the integration token via a
3xx `Location:` header. `reporter_for_project` continues to fall back
to `NoopCiReporter` when construction fails, with a `warn!` log.

Unit tests (`cargo test -p core --tests forge::reporter`):

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

## Forge provider registry

Every per-forge decision (reporter construction, webhook parsing,
signature verification, event classification) lives behind the
`ForgeProvider` trait in `backend/gradient-forge/src/`; the `ForgeRegistry`
maps each `ForgeType` to its provider and is shared via `CiContext.forge`.
Reporter selection and webhook dispatch resolve a provider instead of
matching on `ForgeType`.

Unit tests (`cargo test -p core --tests forge::providers`):

- `gitlab::tests::{matches_token_exactly, rejects_mismatched_token,
  rejects_missing_token}` - constant-time `X-Gitlab-Token` equality.
- `gitea::tests::rejects_missing_signature` - empty `X-Gitea-Signature`
  is rejected.

## GitLab outbound CI reporter (#90)

`GitlabReporter` (in `backend/gradient-forge/src/reporter.rs`) posts commit
statuses to GitLab via `POST {base_url}/api/v4/projects/{id}/statuses/{sha}`,
where `id` is the URL-encoded `owner/repo` path (also covers nested
groups such as `group/sub/repo`). Authenticates with `PRIVATE-TOKEN`,
which accepts personal, project, and group access tokens. The
`ForgeStatusReport` action dispatcher in `backend/gradient-ci/src/actions.rs`
resolves the integration row and constructs a `GitlabReporter` (or the
appropriate forge-specific reporter) per dispatch - the legacy per-project
lookup helper has been removed.

Unit tests (`cargo test -p core --tests forge::reporter`):

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

`decrypt_ssh_private_key` in `backend/gradient-sources/src/ssh_key.rs`
decrypts the per-organization SSH key from `organization.private_key`.
Decryption failure must NOT silently fall back to interpreting the
stored value as a plaintext PEM, otherwise anyone with write access to
that column could bypass encryption entirely.

Tests (`backend/gradient-sources/src/ssh_key.rs`):

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
- `backend/gradient-storage/src/source_nar.rs` - in-file unit tests for
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
`CacheSigner` (in `backend/gradient-sources/src/cache_key.rs`) built once
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
and `sign_missing_signatures` calls `gradient_util::nix_hash::normalize_nar_hash`
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
(`backend/gradient-db/src/pool.rs`). Both newtypes forward `ConnectionTrait`
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

`backend/gradient-util/src/http.rs` builds the project-wide client with sane
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

Unit tests in `backend/gradient-util/src/http.rs`:

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

`backend/gradient-util/src/shutdown.rs` introduces a `Shutdown` primitive bundling a
`tokio_util::sync::CancellationToken` with a `tokio_util::task::TaskTracker`.
It replaces bare `tokio::spawn` for every long-lived background task -
dispatch loops, the outbound worker connection loop, the cache GC and
sign-sweep loops, webhook deliveries, CI reporters, and the fire-and-forget
metric writes from the NAR cache surface. `serve_web` installs a
SIGINT/SIGTERM handler that calls `shutdown.cancel()`, hands the token to
`axum::serve(...).with_graceful_shutdown(...)`, then awaits
`shutdown.cancel_and_drain(30s)` so in-flight cleanups, metric writes, and
webhook deliveries finish before the process exits.

Unit tests in `backend/gradient-util/src/shutdown.rs`:

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

`backend/gradient-db/src/dependency_graph.rs` exposes
`collect_transitive_dependents`, the single canonical reverse-edge BFS over
the `derivation_dependency` table. Both the cache-invalidation closure
revocation in `cache::cacher::invalidate::revoke_cache_derivation_closure`
and the build-failure cascade in
`scheduler::build::BuildStateHandler::cascade_dependency_failed` now route
through it instead of carrying their own copy. The cascade also collapses
to a single batched `derivation IS IN (...)` builds query, replacing the
prior per-iteration full re-scan + per-build edge probe.

Unit tests in `backend/gradient-db/src/dependency_graph.rs`:

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
- `Default` resolves to `Uuid::nil()` so `Id::default() == Id::nil()` -
  enables `Model { id, ..Default::default() }` ergonomics.

## Model defaults (`entity::model_default_tests`)

Every `DeriveEntityModel` struct derives `Default`, and every
`DeriveActiveEnum` column type has a `#[default]` variant (initial-state /
fail-noisy where applicable). Smoke tests in `backend/entity/src/lib.rs`
confirm the derive resolves for representative models:

- `user::Model::default()` - strings empty, `id` is nil, no password.
- `build::Model::default()` - `status == BuildStatus::Created`.
- `evaluation::Model::default()` - `status == EvaluationStatus::Queued`.
- `audit_log::Model::default()` - JSON metadata is `None`, timestamp is
  the 1970 epoch from `NaiveDateTime::default()`.
- `organization_cache::Model::default()` - `mode == ReadWrite`.

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

The `graph_stuck` reason (pool can build everything but the dependency-closure
gate leaves nothing dispatchable) round-trips in
`backend/gradient-types/src/waiting_reason.rs::graph_stuck_round_trip`
(`kind: "graph_stuck"`, `pending_anchors`). Its scheduler trigger -
`build_phase_decision` detecting a `workers` verdict with empty `unmet`, running
`requeue_failed_closure_for_eval` + `reconcile_closure_complete` + `promote_ready`,
then re-assessing to `Building` or parking `graph_stuck` - is exercised end-to-end
in CI (the db crate has no real-Postgres unit harness). The frontend renders it in
`evaluation-log.component.spec.ts::titles and explains a graph-stuck stall`
(`waitingTitle` "Recovering Build Graph", `formatWaitingReason` blocked count).

`requeue_failed_closure_for_eval` is the load-bearing addition for the most common
stuck case: a transitive dependency a *prior* eval left terminal-failed
(`DependencyFailed`/etc.), which this eval pruned out so it carries no `build_job`
here. `resolve_anchors` only requeues the eval's re-reported derivations
(`all_drv_ids`), so such a dep stays failed, blocks its dependents
(`etc`->`activate`->`nixos-system` observed live on `system-units`), and since the
gate correctly refuses to dispatch it, nothing fails to trigger the reactive heal.
The recursive-closure requeue walks `derivation_dependency` down from the eval's
anchors and resets every `4/5/6/9` node to `Created`, so promotion (keyed on any
`build_job`, not this eval's) rebuilds the failed subtree bottom-up.

`reconcile_cached_anchors_for_eval` closes the underlying class: the dispatch gate
keys on build-graph anchor state (`status` + `closure_complete`), which repeatedly
desyncs from the durable cache state. A requeue / dependency-failed cascade / demote
resets an anchor whose **outputs are all still in our cache** (`cached_path.file_hash`)
to `Created`/`DependencyFailed`, so it satisfies neither gate arm and blocks its
dependents though its artifacts exist (observed live: `tzdata-2026b` with all four
outputs cached, anchor `status=0`/`closure_complete=f`, blocking the `etc` chain).
Cache presence is the ground truth for "is this built", so the reconcile (over the
eval's closure, in both the graph-stuck heal and `handle_eval_job_completed`) marks
every fully-cached anchor `Completed` + `closure_complete`. The rare case where a
cached output's runtime closure is itself incomplete is left to the reactive heals
(`demote_referrers_of` / absent-orphan recovery) as the backstop. Exercised
end-to-end in CI (the db crate has no real-Postgres unit harness).

## Pre-build evaluation stall when no worker exists (issue #97)

`backend/gradient-db/src/state_machine/eval.rs::tests` extends the evaluation
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

## Server startup: recover interrupted work (`gradient-db`)

`gradient_db::recover_interrupted_work` (`backend/gradient-db/src/recovery.rs`) runs at
`serve_web` startup to reconcile work left in-flight by the previous process: aborts
`Running` `build_attempt` rows, re-queues `Building` builds, aborts `Fetching` /
`EvaluatingFlake` / `EvaluatingDerivation` evaluations, and sets `force_evaluation` on
their projects. `Building` evaluations and all terminal states are not touched.

Unit tests in `backend/gradient-db/src/recovery.rs` (MockDatabase):

- `all_four_operations_populate_report` - feeds mock `rows_affected` for each step and
  asserts the `RecoveryReport` fields match.
- `project_force_step_skipped_when_no_pre_build_evals` - empty eval SELECT causes steps
  3b/3c to be skipped; report fields all zero.

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

## Substituted at build time - outputs already valid on the worker (#303, #399)

When the daemon reports a build's outputs as already valid (empty
`built_outputs`), no build actually ran. The worker sets the `substituted`
flag on its `BuildOutput` job update. `handle_build_output` records that flag
on `build.substituted` but leaves the build in `Building`: it must not become
terminal here, because the worker pushes the output NARs only at the end of
the job (just before `JobCompleted`). Flipping to `Substituted` on
`BuildOutput` made the build dispatch-ready while its bytes were still absent
from the cache, so a dependent dispatched into that window (the incremental
mid-eval dispatcher, #392/#399) failed `InputsUnavailable`.
`handle_build_job_completed` reads the flag and finalises the terminal status
(`Substituted` vs `Completed`) after the push. This is distinct from eval-time
`compute_truly_substituted` (above), which covers outputs already cached
before evaluation.

Tests:

- `terminal_status_is_substituted_only_when_outputs_were_already_valid`
  (`scheduler`) - `terminal_success_status(true)` is `Substituted`,
  `(false)` is `Completed`; the single source of truth for the completion
  status, decided from the persisted flag.
- `build_sm_building_to_substituted` (`core`) - the `Building → Substituted`
  transition is permitted by the state machine.
- `build_output_substituted_records_flag_without_terminal_transition`
  (`scheduler`) - `handle_build_output` with `substituted = true` writes
  `build.substituted` and leaves the build in `Building`; it does not run the
  terminal status transition.
- `build_completed_finalizes_substituted_from_flag` (`scheduler`) - a
  `Building` build whose `substituted` flag is set finalises as `Substituted`
  on `JobCompleted` (an actual `Building → Substituted` UPDATE), and the
  evaluation finalises as `Completed`.

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
`Shutdown::spawn` (`backend/gradient-util/src/shutdown.rs`) wraps every spawned
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

Tests in `backend/gradient-types/src/cli/registration.rs` cover the DSN override helper:

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
self-signed CA installed in the OS trust store work - fix for #287) and
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
- `core/src/forge/webhook.rs` - extraction of
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
- `core/src/forge/reporter.rs::{gitea,github,gitlab}_comment_url_*`
  (issue #274) - per-forge URL builders for the `post_pr_comment`
  trait method that surfaces wildcard parse errors back to the
  commenter.
- `core/src/forge/reporter.rs::forge_comment_payload_serializes_with_body_field`
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

### Backend - REST endpoints (`backend/web/tests/actions.rs`)

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

### Backend - dispatcher unit tests (`backend/core/tests/actions_dispatch.rs`)

Run with: `cargo test -p core --test actions_dispatch`

- `matches_event_returns_true_for_listed_event` - action fires when its `events` list contains the incoming event.
- `matches_event_returns_false_for_unlisted` - action does not fire for events not in its list.
- `matches_event_empty_events_never_fires` - empty `events` list → no dispatch.
- `forge_status_ignores_events_list` - `forge_status_report` always maps `build.started/completed/failed` regardless of `events`.
- `payload_helpers_include_all_fields` - outgoing JSON payload for `send_web_request` contains `event`, `project`, `organization`, `id`, `status`.

### Backend - inline unit tests (`backend/gradient-ci/src/actions.rs`)

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

- `backend/gradient-db/src/permissions.rs` - `CachePermission` bitmask unit tests
- `backend/web/src/access.rs` - `load_cache` access matrix tests
- `backend/web/tests/cache_roles.rs` - role CRUD endpoint tests
- `backend/web/tests/cache_members.rs` - member CRUD endpoint tests
- `backend/web/tests/cache_subscription_gate.rs` - bilateral subscription tests
- `backend/web/tests/cache_api_key_pinning.rs` - cache-pinned API key tests

## Admin tasks & deep GC (issue #271)

- `backend/gradient-db/src/admin_tasks.rs` - DB helper unit tests: insert/find/mark transitions, unique-violation detection, startup recovery `mark_all_active_failed`.
- `backend/cache/src/cacher/deep_gc.rs` - sweep unit tests: blob pass removes orphan blob, blob pass purges zombie row, log pass removes orphan log, `DeepGcReport` serialises with snake_case keys.

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

- `check_project_updates_propagates_unreachable_remote_error` - `git://127.0.0.1:1/…`
  triggers an immediate connection-refused; the helper now returns `Err(SourceError)`
  instead of `Ok((false, vec![]))`. Locks in the propagation guarantee that the
  endpoint relies on for its 4xx mapping.

### Frontend

Run with: `pnpm --dir frontend exec ng test --watch=false`

- `project-detail.component.spec.ts → 'shows an inline error banner when
  startEvaluation fails'` - mocks `ProjectsService.startEvaluation` to throw;
  asserts the `.evaluation-error` banner renders the underlying message.
- `project-detail.component.spec.ts → 'clears the error banner when the user
  retries'` - calling `dismissError()` resets `errorMessage()` to `null` and the
  banner disappears.

## Flake `git+` scheme breaks repository polling (#427)

A repository URL stored in its nix flake form (`git+https://host/org/repo.git`)
passes validation but failed evaluation with `Git command failed: invalid
argument port`. libgit2 registers no `git+https`/`git+http` transport, so it
misroutes such URLs to SSH, whose scheme has no default port, and the connect
aborts. The libgit2 polling paths (`ls_remote_head`, `commit_info`) now strip the
`git+` prefix via `git::url::git_transport_url` before handing the URL to
libgit2, mirroring what `parse_nix_git_url` already does for the SSH prefetch
path. Bare schemes and SCP-style remotes pass through unchanged.

Run with: `cargo test -p gradient-sources --lib git::tests::git_transport_url`

- `git_transport_url_strips_git_plus_https` - `git+https://…/repo..git` →
  `https://…/repo..git` (the exact URL from the issue).
- `git_transport_url_strips_git_plus_http_and_ssh` - `git+http://` and
  `git+ssh://` lose the prefix too.
- `git_transport_url_passes_through_bare_schemes_and_scp` - `https://`, `git://`,
  and `git@host:path` are returned verbatim.

## Gitea test webhook rejected as malformed (#428)

Gitea's "Test Delivery" button sends a push event with an all-zero `after` SHA
(the same shape a real branch/tag deletion has). `decode_push_commit` returns
`None` for that, and the endpoint conflated it with a genuinely unparseable
payload, answering `400 malformed webhook payload` - which makes the forge mark
the webhook as failing. Push parsing now returns a `PushOutcome`: `None` only for
unparseable JSON (still `400`), `Ignored` for a well-formed but non-buildable
delivery (all-zero SHA), and `Build` for a real push. The endpoint maps `Ignored`
to a `200` empty `WebhookResponse` (`event: "push"`, no queued/skipped).

### gradient-forge

Run with: `cargo test -p gradient-forge --lib webhook`

- `gitea_test_webhook_zero_sha_is_ignored_not_malformed` - the exact issue
  payload parses to `Some(PushOutcome::Ignored)`, not `None`.
- `github_branch_deletion_zero_sha_is_ignored` - a real branch deletion is also
  a no-op, not an error.
- `push_with_unparseable_json_is_malformed` - non-JSON and `{}` still yield
  `None` (the `400` path).
- `github_push_extracts_commit_subject_and_author`,
  `github_push_without_head_commit_has_no_message`,
  `gitlab_push_picks_commit_matching_after` - real pushes still parse to
  `PushOutcome::Build` (updated for the new return type).

### gradient-web

Run with: `cargo test -p gradient-web --test forge_hooks`

- `forge_webhook_test_ping_zero_sha_is_ok_noop` - posting the all-zero Gitea
  push to `/hooks/gitea/{org}/{name}` (valid signature) returns `200` with an
  empty `push` response and touches no trigger/project rows.

## Source-IP allowlist (#282)

### Backend

Run with: `cargo test -p gradient-web --test ip_allowlist`

- `empty_list_allows_everything` - empty allowlist is a permissive default so
  existing rows keep working after migration.
- `slash_32_exact_match`, `slash_24_contains_address` - exact-host and net-mask
  containment.
- `ipv4_mapped_ipv6_matches_ipv4_cidr` - dual-stack sockets compare correctly.
- `malformed_entry_is_skipped_but_others_still_count` - validation happens at
  the API edge; the runtime check tolerates noise.
- `normalize_bare_ipv4_to_slash_32` / `normalize_bare_ipv6_to_slash_128` /
  `normalize_keeps_cidr_unchanged` / `normalize_trims_whitespace` /
  `normalize_rejects_garbage` / `normalize_rejects_empty` - write-time canonicalization.

## Upstream cache types + Gradient Proto (#118)

- `cargo test -p entity --lib cache_upstream` - `as_source` for internal/gradient_proto/http + inconsistent rows.
- `cargo test -p core --lib db::cache_upstream` - http vs gradient_proto upstream resolution.
- `cargo test -p core --lib sources::secret` - encrypt/decrypt roundtrip for stored credentials.
- `cargo test -p web --lib endpoints::caches::upstreams` - per-type validation error messages, plus `validate_gradient_proto_requires_https_when_api_key_present` (an API key forces an `https://` upstream) and `validate_gradient_proto_rejects_unsafe_remote_cache` (remote cache name restricted to a safe charset).
- `cargo test -p proto --lib handler::cache` - cache-scoped query + Push rejection.
- `cargo test -p proto --lib handler::cache_session` - read-only message allow-list.
- `cargo test -p proto --lib handler::limiter` - per-IP connection cap.
- `cargo test -p proto --lib handler::cache_consumer` - ws URL building.

## Fetch- and eval-capability gating for flake jobs (#252)

A `FlakeJob` carrying a `FetchFlake` task clones its source repository (over
SSH for private repos), and the server only sends SSH credentials to
fetch-capable workers. A task carrying `EvaluateFlake`/`EvaluateDerivations`
spawns the Nix eval subprocess, which a non-eval worker is not provisioned for:
a fetch+build worker handed a bundled job ran the eval anyway and got
SIGKILL-ed (OOM). The scheduler (`WorkerCaps::can_eval`) now gates flake jobs
on `fetch` for the fetch task and `eval` for any evaluation task, so a bundled
job requires both.

Run with: `cargo test -p scheduler --lib jobs`

- `can_eval_requires_eval_for_evaluation_tasks` - the pure capability check:
  bundled jobs need both `fetch` and `eval`, cached-eval jobs need `eval`,
  fetch-only jobs need `fetch`.
- `fetch_flake_job_requires_fetch_capability` - a bundled flake job is assigned
  only to a fetch- and eval-capable worker, not one lacking `fetch`.
- `bundled_eval_job_skips_worker_without_eval` - a fetch+build worker (no
  `eval`) is not offered a bundled `FetchFlake`+evaluate job.
- `cached_eval_job_requires_eval_not_fetch` - an eval-only follow-up job
  (cached source, no `FetchFlake`) runs on an eval-capable worker without
  `fetch`.

## Adaptive fetch/eval split

When an idle dedicated eval worker is connected, the scheduler dispatches a
fetch-only flake job to a fetch worker and hands evaluation to the eval pool via
a cached-source follow-up; a scoring penalty keeps fetch workers free. The eval
worker substitutes the cached source from the binary cache before evaluating.

Run with: `cargo test -p scheduler --lib` and `cargo test -p worker --lib`

- `worker_pool::tests::idle_eval_only_worker_detected` /
  `draining_eval_only_worker_does_not_count` - the split heuristic (an idle,
  non-draining eval-only worker triggers the split).
- `jobs::tests::is_fetch_only_true_only_for_fetch_task_alone` - recognises a
  fetch-only job by its task list.
- `jobs::tests::cached_followup_rewrites_source_and_tasks` - builds the cached
  eval follow-up (Cached source, eval tasks, source as a required path).
- `scheduler_tests::fetch_only_completion_enqueues_cached_eval_followup` - a
  completed fetch-only job enqueues the cached eval follow-up reusing its id.
- `policy::tests::reserve_rule_penalizes_fetch_worker_for_cached_eval_only` -
  fetch workers are penalised for cached-eval jobs, eval-only workers are not.
- `executor::eval::tests::cached_source_requires_store_path_present` - the
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

- `trigger::tests::gate_same_repo_pr_bypasses` - same-repo PR runs without a gate.
- `trigger::tests::gate_fork_untrusted_sender_parks` - fork PR with an
  untrusted sender parks for approval (carrying PR number/author).
- `trigger::tests::gate_fork_trusted_sender_bypasses` - a trusted maintainer
  (force-push / command) bypasses the gate.
- `trigger::tests::gate_unknown_fork_status_fails_closed` - uncertain fork
  status with an untrusted sender parks (fail-closed).
- `events::tests::github_pr_sender_distinct_from_author_on_force_push` /
  `gitea_pr_parses_sender_login` / `gitlab_mr_sender_falls_back_to_event_user`
  - the event actor is parsed independently of the PR author.
- `reporting::tests::evaluation_context_format_with_custom_wildcard` - custom
  wildcard produces `gradient/{project}: Evaluation: {wildcard}`.

## Evaluation check goes green when the eval finishes, not all builds (#453)

The forge Evaluation check used to flip green only on `evaluation.completed`,
which fires after every build finishes. It now succeeds at `evaluation.building`
(the moment the eval phase concludes and builds start); per-build checks carry
build outcomes. Once `evaluation.building_started_at` is set, a later
Failure/Error on the Evaluation context is suppressed so a build failure or a
user abort cannot redden the already-green eval check.

Run with: `cargo test -p gradient-ci --lib` and
`cargo test -p gradient-core --test actions_dispatch`.

- `reporting::tests::suppresses_eval_failure_only_after_building` -
  `suppress_evaluation_failure` returns true for Failure/Error only when
  `reached_building`, and never for Success/Pending.
- `actions::tests::forge_status_mapping` /
  `actions_dispatch::forge_status_mapping_complete` -
  `evaluation.building → Success`.
- `actions::tests::matches_event_forge_status_ignores_stored_events` -
  `forge_status_report` actions also match the `evaluation.building` event.

## GitHub installations declared as `github` integrations (#453)

State-managed GitHub installations live in the `integrations` map as
`forge_type=github` entries carrying an `installation_id` (no secret/token), not
a separate top-level resource or an org-state list. Apply upserts the
`github_installation` row and links it from the integration; export round-trips
the `installation_id`/`account_login`; validation requires a positive
`installation_id` for github entries.

Run with: `cargo test -p gradient-state`.

- `config::config_tests::deserializes_github_integration_with_installation_id` -
  a `forge_type=github` integration deserialises with `installation_id` +
  optional `account_login`.
- `tests::state_github_integration_requires_installation_id` - a github
  integration with no `installation_id` fails validation on
  `integrations.{name}.installation_id`.
- `tests::state_github_integration_with_installation_id_is_valid` - the same
  entry with a positive `installation_id` validates.

## Cache upload - NAR ingest, endpoint, connector, and CLI (issue #261)

### Shared NAR ingest (`gradient_proto::ingest`)

Run with: `cargo test -p gradient-proto ingest`

- `malformed_store_path_bails_before_any_io` - a syntactically invalid store
  path is rejected before any blob write is attempted.
- `create_path_writes_blob_and_reports_created` - a valid NAR + narinfo pair
  writes the blob to storage and returns `IngestResult::Created`.

### Upload endpoint (`web` crate)

Run with: `cargo test -p web --test caches_upload`

- `upload_unauthenticated_returns_403` - `POST /api/v1/caches/{cache}/nars`
  without a bearer token returns `403`.
- Real-DB integration stubs are present but marked `#[ignore]`; they run in
  CI against a live Postgres instance.

### Connector multipart upload (`connector` crate)

Run with: `cargo test -p connector nar_upload`

- `nar_upload_posts_multipart` - the connector assembles the correct multipart
  form (a `narinfo` JSON part and a `nar` binary part) and maps a 200 response
  to success.

### CLI narinfo parser

Run with: `cargo test -p gradient-cli`

- `parses_full_narinfo` - a complete `.narinfo` file round-trips through the
  parser with all fields populated.
- `missing_required_field_errors` - a narinfo missing a required field (e.g.
  `StorePath`) returns a parse error naming the field.
- `empty_references_ok` - a `References:` line with no paths is accepted and
  produces an empty references list.

### CLI `cache_upload` integration

Run with: `cargo test -p gradient-cli`

- `upload_nar_file_with_narinfo_succeeds` - providing both `--nar-file` and
  `--narinfo` drives the chunked upload (`PUT .../nars/{hash}/chunk` then
  `POST .../nars/{hash}/finalize`) against a mock server and returns success.
- `upload_nar_file_without_narinfo_errors` - omitting `--narinfo` in no-nix
  mode exits with a usage error (exit code 2).

### CLI TUI view-model tests

Run with: `cargo test -p gradient-cli`

- `tui::nar_browser` - filter input narrows the displayed list; scroll position
  resets to 0 when the filter changes; clearing the filter restores the full
  list.
- `tui::graph` - expanding a collapsed node adds its children to the visible
  set; collapsing removes them; nested expand/collapse is consistent; `Esc`
  triggers quit.
- `tui::log_view` - `↑`/`↓` scroll adjusts the offset; enabling follow-tail
  pins the view to the last line; `/` search highlights matching lines.
- `tui::watch` (#314) - the `gradient watch` dashboard view-model:
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

- **Auth / user / keys** - check-username, register, login, logout; profile,
  settings, sessions, audit-log, search; API-key create/list/revoke/delete.
- **Organizations** - CRUD, available/public, ssh rotation, roles CRUD, and
  membership (a second user is added via `POST /orgs/{org}/users`, re-roled with
  `PATCH`, and removed with `DELETE`, asserting the member list each time).
- **Projects** - CRUD, details, triggers, active toggle, plus a transfer flow
  that moves a throwaway project to a second org and verifies it disappears from
  the source and appears under the destination.
- **Workers** - register/list/patch/delete (direct + CLI), with v4 worker UUIDs.
- **Caches** - CRUD, key/stats, active/public toggles, plus sub-resources:
  member add/re-role/remove, custom-role create/get/patch/delete, an HTTP
  upstream create/patch/delete, and org subscription remove/restore.
- **Cache NARs** - synthetic upload (CLI + direct multipart), list/show/stats/
  available, and delete (CLI plus a direct `DELETE` asserting `204`).
- **Build-dependent endpoints** - exercised on empty state for correct
  not-found behaviour, since no builds are present.
- **Edge cases** - duplicate creates (org, project, cache, org/cache role, API
  key, org/cache member, subscription) return an enveloped `409`; a reserved
  project name (`build-request`) and an empty API-key permission mask return
  enveloped `400`s.
- **Permissions (multi-actor)** - the second user acts with their own token: a
  non-member cannot read the private org; the built-in `View` role grants read
  but is rejected (enveloped `403`) on settings edit, project create, member
  add, and org delete; promotion to `Admin` unlocks the settings edit.
- **State export (`GET /admin/state`)** - rejected (`403`) for a non-superuser;
  after elevating `operator` to superuser in the DB, the JSON format returns the
  seeded org/project/cache with secret `*_file` fields redacted to `null`, and
  the default Nix format renders the same resources as a pasteable expression.

The auth surface is rate-limited (burst 5, one token per 6s), so the script
spaces its registration/login calls to stay within the bucket.

Out of scope (covered by dedicated tests or requiring external services):
OIDC, SMTP e-mail verification, forge webhooks, the worker proto protocol,
the Nix binary-cache serving family, and build-request dispatch.

## State export endpoint (#188)

`backend/gradient-state/src/export.rs` unit tests cover the secret-redaction pass
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

### `Derivation::build_meta()` parsing - `core/src/db/derivation.rs`

Run with: `cargo test -p core --lib db::derivation`

- `build_meta_reads_all_fields` - all four attributes (`timeout`,
  `maxSilent`, `preferLocalBuild`, `requiredSystemFeatures`) are parsed
  into a `BuildMeta` with the correct values.
- `build_meta_defaults_when_absent` - a derivation with none of the
  attributes returns all-default `BuildMeta`.
- `build_meta_prefer_local_build_accepts_true_and_1` - both `"true"` and
  `"1"` are accepted as `prefer_local_build = true`.
- `build_meta_ignores_unparseable_timeout` - a non-integer `timeout`
  attribute falls back to `None` instead of erroring.

### Build state-machine transitions - `core/src/state_machine/build.rs`

Run with: `cargo test -p core --lib state_machine::build`

- `build_sm_building_to_failed_transient` - `Building → FailedTransient`
  is a valid transition (worker classified the failure as transient).
- `build_sm_failed_transient_to_queued_for_retry` - `FailedTransient →
  Queued` is valid (scheduler re-queues for the next attempt).
- `build_sm_failed_transient_to_permanent_when_exhausted` - `FailedTransient
  → FailedPermanent` is valid (attempt budget exhausted).
- `build_sm_failed_transient_is_not_terminal` - `FailedTransient` is not
  terminal; the state machine permits outgoing edges from it.
- `build_sm_failed_permanent_and_timeout_are_terminal` - `FailedPermanent`
  and `FailedTimeout` are terminal; no outgoing transitions are accepted.
- `build_sm_terminal_failure_rejects_requeue` - attempting to transition
  either terminal failure status back to `Queued` is rejected.
- `build_sm_building_to_substituted` - `Building → Substituted` is valid, so
  a worker that finds the outputs already valid can finalize the build as
  `Substituted` rather than `Completed` (issue #303).

### Retry decision and backoff - `scheduler/src/build.rs`

Run with: `cargo test -p scheduler --lib build::retry_tests`

- `permanent_is_terminal_regardless_of_attempt` - `FailedPermanent` is
  never retried regardless of the current attempt count.
- `timeout_is_terminal` - `FailedTimeout` is never retried.
- `transient_retries_until_budget_then_permanent` - `FailedTransient`
  retries while attempts remain; once the budget is exhausted the outcome
  is `FailedPermanent`.
- `backoff_grows_per_attempt` - the retry delay doubles with each attempt
  (exponential backoff).
- `substitute_unavailable_requeues_penalty_free` - a `SubstituteUnavailable`
  failure always maps to `FailureOutcome::Requeue` (back to `Queued`, no
  `attempt` bump), regardless of the attempt count.
- `substitute_miss_requeues_but_real_failures_cap_at_three` - documents the
  interaction: substitute misses never consume the attempt budget (always
  `Requeue`), while real transient failures hit `FailedPermanent` at attempt 2
  (with `build_max_attempts = 3`).

### Substitute-miss escalation - `scheduler/src/build.rs`

Run with: `cargo test -p scheduler --lib build::waiting_reason_tests`

- `substitutable_below_threshold_is_buildable_anywhere` - a substitutable build
  under `SUBSTITUTE_MISS_ESCALATION_THRESHOLD` misses is buildable-anywhere
  (substitute mode) and never appears in the waiting reason.
- `substitutable_at_threshold_escalates_to_real_arch_check` - once a
  substitutable build reaches the threshold it is checked against its real
  arch/features; with no matching arch worker it is not buildable-anywhere and
  surfaces as an unmet requirement so the parker can park the eval.

### Substitute-miss state transition - `gradient-db/src/state_machine/build.rs`

- `build_sm_building_to_queued_for_substitute_requeue` - a `Building` substitute
  attempt may transition back to `Queued` (penalty-free re-queue).

### Per-build limit resolution - `scheduler/src/dispatch.rs`

Run with: `cargo test -p scheduler --lib dispatch::limit_tests`

- `per_drv_overrides_default` - a non-zero per-derivation limit takes
  precedence over the server default.
- `zero_means_no_limit` - a stored value of `0` is treated as no limit
  (`None`), not as `0`.
- `falls_back_to_default_when_absent` - when no per-derivation value is
  present, the server default is used.

### Worker failure classification - `worker/src/executor/build.rs`

Run with: `cargo test -p worker --lib executor::build::classify_tests`

- `builder_nonzero_is_permanent` - a non-zero builder exit code maps to
  `BuildFailureKind::Permanent`.
- `oom_signature_is_transient` - a log line matching the OOM heuristic
  maps to `BuildFailureKind::Transient`.

A substitute miss (`external_cached` build whose `fetch_external_cached_outputs`
fails) reports `BuildFailureKind::SubstituteUnavailable` and never falls back to
a local build - see `BuildError::substitute_unavailable`.

### Entity helpers - `entity/src/build.rs`

Run with: `cargo test -p entity --lib build`

- `is_failure_covers_all_failure_states` - `FailedPermanent`,
  `FailedTransient`, and `FailedTimeout` all return `true` from
  `is_failure()`.
- `terminal_failure_excludes_transient` - `FailedTransient` returns
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

### Shared closure-size helper - `backend/gradient-db/src/closure.rs`

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

- `cpu_core_score_in_bounds_and_positive` - the deterministic single-core
  micro-benchmark (`cpu_core_score`) always returns a value in `1..=100_000`.
- `host_static_reports_nonzero` - `host_static` reports at least one CPU and at
  least 1 MiB of total RAM.

`host_static` (logical CPU count, total RAM) is sampled once and advertised via
`WorkerCapabilities`; `host_dynamic` (available RAM, global CPU usage) is sampled
each heartbeat off the dispatch thread and sent via `WorkerMetrics`.

## History-based prediction - issue #304 (Phase 5.3)

Backend (`cargo test -p scheduler --tests history::`):

- `buckets_are_log2_of_mb` - `closure_bucket` maps closure bytes to a
  log2-of-megabytes bucket (1 MiB → 0, 4 MiB → 2, 1000 MiB → 9).
- `empty_rows_yield_default` - `summarize` over no rows returns the zeroed
  `HistoryPrediction` (samples 0).
- `summarize_aggregates_peak_cpu_and_oom` - peak RAM is the max of non-null
  samples (few-sample fallback for p95), CPU time is the mean of non-null
  samples, and `oom_rate` is the fraction of OOM-killed rows.
- `bucket_bounds_widen_by_one_bucket_each_side` - the byte bounds passed to the
  `derivation_metric` query span ±1 closure bucket around the target size.

`scheduler::history::predict` queries the most recent 200 `derivation_metric`
rows for a `pname` (narrowed to comparable closure sizes when known), index-served
by `idx-derivation_metric-pname-closure_size`. `BuildDispatchMaps` preloads one
prediction per candidate derivation outside the scoring lock; `take_best_of_kind`
feeds each build's prediction to the lazy `history` provider. On build completion,
`BuildStateHandler::record_metrics` inserts a `derivation_metric` row from the
worker's `BuildMetrics` and adopts the worker-measured `build_time_ms`.
`summarize` also averages the rows' `build_time_ms` into
`HistoryPrediction.build_time_ms` (same null-filter/mean as `avg_cpu_time_ms`),
which `take_best_of_kind` uses to work-weight each org's active-build share.

### Negative-total dispatch gate (`scheduler::jobs`)

`take_best_of_kind` now refuses to dispatch the best candidate when its total
score is `< 0` (e.g. a build still awaiting candidate scores, which the
`RescoreWaitRule` drives to `-1000`); the worker idles that round instead.

- `dispatch_skips_all_negative` - an unscored build (no `missing_nar_size`,
  `rescore_count` 0) totals negative, so the gate returns `None` and leaves it
  pending.
- `dispatch_picks_non_negative` - a fully-cached build (`missing_nar_size` 0)
  earns the `MissingNarSizeRule` bonus and is dispatched.
- `unscored_build_is_gated_until_scored` - the same build is gated until the
  worker reports zero missing paths, then assigns.

## Log compression, chunking, limiting & store-fetch (#246)

Completed build logs are zstd-compressed into line-bounded chunks at finalize and
served lazily by chunk, line range, or streaming search. Workers cap log
throughput with two token buckets and fetch nix-store logs for already-built
derivations.

**`core::storage::sgr` (`SgrState`)** - ANSI SGR carry-forward: `to_prefix()` is
empty for the default state, reconstructs an active foreground colour, clears on
reset (`\e[0m`), combines bold+colour minimally, handles 256-colour sequences,
and ignores an incomplete escape at end of input.

**`core::storage::log_chunk`** - `chunk_log` splits on line boundaries respecting
the byte target, keeps an over-long line whole, carries the active colour as each
chunk's `color_prefix`, and yields no chunks for an empty log.
`compress_and_store_chunks` zstd-encodes each chunk, writes it via `LogStorage`,
and the round-trip (`read_chunk` → `zstd::decode_all`) reproduces the chunk text.

**`core::storage::log` chunk objects** - `write_chunk`/`read_chunk`/`delete_chunks`
round-trip on `FileLogStorage`; `read` reassembles from chunks once the inline log
is dropped (`delete_inline_log`), so full-log reads and dedup keep working.

**`worker::executor::log_limit` (`LogRateLimiter`)** - admits bytes under the
limit, trips permanently on a burst (1-minute bucket exhausted), trips on the
sustained (1-hour) bucket even when the burst bucket would allow, and refills the
minute bucket over elapsed time while not yet tripped.

**`worker::nix::log`** - `store_log_path` computes
`$NIX_LOG_DIR/drvs/<first2>/<rest>.bz2` from a drv store path (and a bare
basename); `read_store_build_log` bzip2-decodes the stored log or returns `None`.

**`web::endpoints::builds::log_chunks`** - `parse_line_range` accepts `start`/`end`,
defaults the start to 1, parses `L120-L130` and bare `3-8`, and rejects malformed
ranges. (The chunk/line/search endpoints' full request/response behaviour is
covered by CI integration tests, not run locally.)

**Frontend `log-window`** - `parseLineFragment` parses `#L`-style deep-link
fragments and rejects garbage/non-positive; `chunkIndexForLine` maps a line to its
chunk index (or `-1`); `windowAround` centres and clamps a fetch window to the log
bounds and handles empty logs. Run a single spec with
`pnpm exec vitest run <file> --globals --environment node`.

**CLI** - `gradient builds log <id>` keeps streaming parity (the server's
`GET /log` reassembles chunks); `--lines L120-L130` fetches a line range and
`--search <term>` streams matches.

## Runtime closure (#338)

**`core::db::runtime_closure`** - `parse_reference_hash` strips the `-name`
suffix from a `hash-name` reference token; `runtime_closure_reachable` walks
`cached_path.references` and sums NAR sizes, deduping diamonds and returning zero
for empty seeds.

**`web::endpoints::builds::closure::runtime_closure_graph_sums_and_links`** -
`build_runtime_closure_graph` sums sizes, keys nodes by store-path hash, and
emits `source` → `target` edges (referrer depends on reference).

**Frontend `closure-graph`** - the view requests the runtime closure by default
and the build-time closure only under `?type=build` (eval scope uses the eval
runtime endpoint).

## Cache storage limits (#216)

Per-instance (`GRADIENT_MAX_STORAGE_GB`) and per-cache (`max_storage_gb`, GB,
0 = unlimited) storage caps. When every writable cache for an org has less than
10 MiB headroom, new evaluations park in `Waiting` with `CacheStorageFull`.

- **`core::types::waiting_reason::tests::cache_storage_full_round_trip`** - the
  `CacheStorageFull` reason serialises to `kind=cache_storage_full` and decodes
  back.
- **`core::db::cache_storage::tests`** - `zero_limit_is_unlimited`,
  `headroom_bounded_by_tighter_axis`, `headroom_instance_axis_can_dominate`,
  `both_unlimited_is_max` cover the GB→bytes conversion and the per-cache /
  instance headroom math (the decision input for the gate).
- **`core::ci::apply::tests::storage_gate_ignores_non_queued_eval`** -
  `park_if_storage_full` returns an already-`Waiting` eval untouched, issuing no
  cache queries. The `no_eval_capable_worker_parks_*` flow also exercises the
  gate's not-full pass-through.
- **`core::ci::unpark::tests::unpark_storage_full_requeues_when_headroom_returns`**
  - a `CacheStorageFull`-parked eval is re-queued once the org regains headroom.
- **`core::types::cli::storage::tests`** - `default_max_storage_gb_is_unlimited`
  / `clap_default_max_storage_gb_is_zero` pin the `0` default.
- **`web::endpoints::caches::management::tests`** -
  `validate_max_storage_gb_accepts_zero_and_positive` /
  `validate_max_storage_gb_rejects_negative` cover the API validator. Full
  create/patch/get round-trips of `max_storage_gb` run in CI integration tests.

## Job Board & metrics rework (#343)

Unit tests landed with the implementation:

- **`entity::model_default_tests`** - default-row tests for the new tables
  (`phase_event`, `dispatched_job`, `worker_sample`, `worker_connection`,
  `metric_rollup`) plus the new build/evaluation phase-timestamp columns
  defaulting to `NULL`.
- **`entity::ids`** - `new_metrics_ids_round_trip` covers the new id newtypes.
- **`score::policy::tests::score_detailed_sums_to_total_and_names_rules`** -
  the per-rule `ScoreBreakdown` sums to `score()` and names every rule.
- **`core::types::cli::metrics::tests`** - pipeline config defaults.

Integration coverage to run in CI (DB-backed `axum_test` / `MockDatabase`):

- Phase-timestamp + `phase_event` writes on build/eval status transitions
  (`update_build_status` / `update_evaluation_status`).
- `dispatched_job` row written on assignment with a non-empty `score_breakdown`.
- `worker_connection` open/close and `worker_sample` heartbeat rows.
- Rollup aggregator fact→`metric_rollup` minute buckets + min→hour→day→week
  cascade; retention pruning by age/granularity.
- `GET /metrics/query` and `/board/*` scope masking (superuser vs member vs
  anonymous); `/board/live` event masking.

Frontend specs (vitest, run in isolation): `board.service`, `board-live.service`
reconnect, `metric-chart`, and the live-jobs scoring-breakdown drawer.

## Rollup references build_attempt timestamps

The Build/BuildAttempt split moved `build_started_at`/`build_finished_at` to
`build_attempt`, so the rollup queries spammed `column b.build_finished_at does
not exist`. `rollup::tests` (`cargo test -p gradient-db`) guard the schema
coupling: `build_table_rollups_avoid_moved_columns` asserts no build-table
rollup references the moved columns (counts now bucket by `build.updated_at`),
and `duration_rollup_reads_timestamps_from_build_attempt` asserts
`builds.duration_ms` sources start/finish from the latest `build_attempt`. The
job-board durations heatmap (`board_metrics::get_board_durations_heatmap`) had
the same stale `b.build_finished_at`/`b.build_time_ms` references and now reads
both from the latest `build_attempt` via the same `LATERAL` join.

## Storage gate SUM decodes as BIGINT (#350)

`core::db::cache_storage::tests::file_size_sum_casts_to_bigint` asserts the
generated SQL wraps `SUM(file_size)` in `CAST(... AS bigint)`. Postgres widens
`SUM(int8)` to `NUMERIC`, which failed to decode into `Option<i64>` and surfaced
as an Internal Server Error when manually triggering an evaluation for an org
with a writable cache.

## Multi-line evaluation warnings preserved (#351)

`worker::nix::eval_worker::tests::parse_warnings_keeps_multiline_warning` and
`parse_warnings_splits_distinct_and_drops_sqlite_busy` cover the captured-stderr
parser: a `warning:` line plus its following lines (until the next
`warning:`/`trace:`/`error:`/`note:` entry) are kept as one warning, so
multi-line warnings are no longer truncated to their first line, while distinct
warnings still split and the `SQLite database … is busy` line is still dropped.

## Declarative state apply - gradient-api NixOS test (#347)

`nix/tests/gradient/api` declares a full `services.gradient.state` (users with a
superuser, organization with members, a state-managed role, a project with
non-default fields and polling+time triggers, a cache with members/role/upstream,
an API key, a worker registration, and inbound+outbound integrations) and
Phase 8d of `test.py` asserts each resource was provisioned by the startup state
actor. It also checks the API key's bearer token authorizes while its
`viewOrg`-only mask rejects a settings mutation, and (for #349) that
`LimitMEMLOCK` is raised and no `mlock failed` warning appears in the journal.

## Chunked `IN` queries under the Postgres param cap (#345)

`core::db::chunked::tests` cover `fetch_in_chunks`: a list at or below `IN_CHUNK_SIZE`
runs a single query, a larger list splits into `ceil(n / IN_CHUNK_SIZE)` chunks each
within the cap while preserving every id, and an empty list runs no query.
`core::db::status::find_active_leaders_tests::chunks_large_id_lists_under_postgres_param_cap`
drives `find_active_leaders` with 70000 derivation ids and asserts no executed
statement binds more than 65535 parameters (regression for the `/evaluate` 500
"too many arguments for query").

## Per-resource live WebSocket filtering (#345)

`web::endpoints::live::tests` cover the channel predicates: the evaluation channel
forwards only events for its `evaluation_id`, the project channel learns evaluation
ids from `evaluation_status_changed` events and then forwards their
`build_status_changed` events (ignoring other projects), and `cache_changed`
serializes to the `{"type":"cache_changed"}` ping.

## Frontend live service (#345)

`frontend .../core/services/live.service.spec.ts` stubs `WebSocket` and asserts
`LiveService.connect(path)` builds the correct `ws(s)://…${apiUrl}${path}` URL,
emits parsed JSON frames, ignores malformed frames, and closes the socket on
unsubscribe.

## Frontend issues batch (#341)

Frontend (`pnpm --dir frontend exec ng test --include='<glob>' --watch=false`):
- `evaluation-log/build-search.spec.ts` - `matchesBuildSearch` is a case-insensitive
  substring match; empty/whitespace query matches all.
- `evaluation-log/evaluation-log.component.spec.ts` - `renderLog` updates
  `logLineCount` (live count on building builds); the sidebar search filters
  `groupedBuilds` by name while preserving the `visibleBuilds` index used for
  keyboard navigation.
- `core/services/admin.service.spec.ts` - `startDeepGc`/`listTasks` hit the
  `admin/maintenance/deep-gc` and `admin/tasks` endpoints; `githubAppConfigured`
  resolves true/false from the credentials probe.
- `board/health/health.component.spec.ts` - the HTTP-routes table is gone, the
  admin tasks table renders, and the GitHub link reflects the configured state.
- `board/live-jobs/live-jobs.component.spec.ts` - the view toggle loads and
  renders the pending-jobs list.

Backend (`cargo test -p scheduler --tests jobs`):
- `pending_snapshot_reports_kind_and_org` - `JobTracker::pending_snapshot` reports
  each pending job's kind, org, and build-id, backing `GET /board/jobs/pending`.

## Scheduler - windowed instance context (#359)

`instance_metrics_loop` recomputes a `score::InstanceContext` snapshot every
`GRADIENT_INSTANCE_METRICS_INTERVAL` seconds (default 30) from
`derivation_metric` + `dispatched_job` over 5m/1h/24h windows and publishes it
lock-free via `arc_swap::ArcSwap`; `try_assign` feeds the live snapshot into
`take_best_of_kind` instead of `InstanceContext::default()`.

Backend (`cargo test -p scheduler --tests instance`):
- `maps_columns_and_counts_into_snapshot` (`scheduler::instance`) - a
  `MockDatabase` replays raw column maps for the two windowed statements;
  asserts each column lands in the right `Windowed`/count field and that the
  in-memory `InstanceCounts` (active/pending builds, total/idle workers) are
  copied through. The FILTER-window and jsonb-extraction SQL is validated in CI
  against Postgres, not by the mock.

## Scheduler - instance-relative scoring rework (#359)

Soft rules return a bounded `[0,cap]` bonus with instance-relative thresholds;
disqualifiers may go negative; `take_best_of_kind` refuses a negative total.

Backend (`cargo test -p score`):
- `missing_nar_size_bounded_bonus` (`score::rules::builtin`) - `None` scores 0,
  a fully-cached job hits the 500 cap, and a huge NAR stays `>= 0` and below the
  cached bonus.
- `dependency_count_capped_at_50` (`score::rules::builtin`) - 100k dependencies
  saturate at the 50 cap.
- `wait_time_longer_wait_scores_higher_but_capped` (`score::rules::builtin`) -
  wait measured from `ready_at` grows monotonically and saturates at the rule
  cap, clearing the anti-starvation budget.
- `rescore_wait_blocks_build_until_threshold_but_never_eval`
  (`score::rules::builtin`) - an unscored build scores `-1000` at
  `rescore_count` 0, drops to 0 at 4, is 0 once `missing_nar_size` is known, and
  is always 0 for eval jobs.
- `fair_share_breaks_tie_at_equal_wait` (`score::rules::fair_share`) - at equal
  wait the quieter org's job outscores the busier org's, fair-share + wait
  combined.
- `aggregate_len1_is_identity_and_multi_reduces` (`score::context`) -
  `BuildContext::aggregate` is identity for one item; multi-item folds
  dependency/closure sums, OR of prefer-local/fixed-output, max/min/mean history
  fields and concatenated derivations.

Backend (`cargo test -p scheduler`):
- `bump_rescore_increments_pending_only` (`scheduler::jobs`) -
  `bump_rescore_counts` raises pending jobs' `rescore_count` but leaves an
  assigned (active) job at 0.

Frontend (`frontend/.../board/job-detail/job-detail.component.spec.ts`) -
structured context panels: renders worker `cpu_count`, a derivations row
(`pname`/`drv_path`), the job-context kind + history `peak_ram_mb`, and the
instance-context windowed table with scalar counts.

## Job & worker context fixes (#366)

The dispatched-job detail now hides the server-only `core`/`cache` capability
flags and the redundant standalone `fetch` row, shows the job-context
architecture only for build jobs, links the worker id to its org-scoped metrics
page (`organization_name` is now part of `DispatchedJobDetail`), makes the
derivation rows the build entry-point, and falls back to a limited view (or a
"Job not found" message) when a queued job has no dispatch record yet. The live
board persists its tab/filter selection in `sessionStorage` and reconciles the
optimistic live rows with the persisted, selectable rows shortly after a
dispatch event.

Frontend (`board/job-detail/job-detail.component.spec.ts`) - capability list
excludes `core`/`cache` and has no separate `Fetch` row; architecture renders
for build jobs and is hidden for eval jobs; the worker id links to
`/organization/{org}/workers/{id}/metrics`; an undispatched job renders the
pending view without a scoring breakdown, and an unknown id renders "Job not
found".

Frontend (`board/live-jobs/live-jobs.component.spec.ts`) - the view/filter
selection round-trips through `sessionStorage`: a persisted `pending` view is
restored on load and switching the tab writes it back.

## Minor frontend & job board fixes (#375)

Frontend tests documenting UI improvements to the action form, live jobs list, and job detail page:

- `action-form.component.spec.ts` - `forge_status_report` actions submit an empty `events` array; submit errors render inside the action dialog.
- `live-jobs.component.spec.ts` - dispatched/pending job rows show the derivation `pname`.
- `job-detail.component.spec.ts` - the "Previous Build Attempts" section renders only when a build has more than one attempt, one linked row per attempt.

## Virtualized live log streaming

The evaluation-log page renders streaming (Building) logs through the same
virtualized window as chunked (Completed) logs, sourced from memory instead of
the chunk API. Each drain tick converts and mounts only the newly received
lines - previously the entire log was ANSI-converted and re-parsed into one
`innerHTML` every 80 ms, which saturated the main thread on high line counts.

Frontend (`evaluation-log/evaluation-log.component.spec.ts`,
`pnpm -C frontend exec ng test --include '**/evaluation-log/*.spec.ts' --watch=false`):
- `appendStreamedLines` updates `logLineCount` (live count on building builds).
- only newly streamed lines run through `convertAnsiToHtml`; the window keeps
  appending with stable line numbers.
- the rendered window is capped at `MAX_WINDOW` lines, with trimmed lines
  represented by the top spacer.
- with auto-scroll off the window stays pinned: new lines only grow the bottom
  spacer and the line count.
- scrolling up during streaming pages older lines from the in-memory log
  (`loadWindow` prepend) with correct numbering and spacer heights.

## Project page redesign - status rollups & dependency counts (#295)

- `backend/gradient-web/src/endpoints/projects/mod.rs::rollup_tests` - `bar_segment` maps every `BuildStatus` to the correct segment, and `BuildStatusCounts::total()` excludes `Substituted`/`Aborted`.
- `backend/gradient-web/src/endpoints/projects/evaluations.rs::tests::first_line_truncated_takes_first_line_and_caps_length` - commit-message first non-blank line extraction and 100-char cap.
- `backend/gradient-web/src/endpoints/live.rs::tests::project_channel_forwards_build_transitions_for_seeded_evals` - the project live channel forwards build transitions for evaluations seeded at upgrade, so segmented bars/queue move while builds run.
- Frontend `segmented-bar.component.spec.ts` - all four segments render with widths proportional to the four-segment total (substituted/aborted excluded; zero counts render at 0% width so live count changes animate); work finished entirely via substitution renders a single full green segment; all-zero counts render a grey track; hovering a segment shows an instant custom tooltip with its count.
- Frontend `project-detail.component.spec.ts` - explicit eval selection is persisted in the `eval` query param (back-navigation restores it) and `barCounts` folds the entry point's own build status into its dep-closure counts.
- `backend/gradient-web/src/endpoints/projects/evaluations.rs::tests::checked_at_maps_null_time_sentinel_to_none` - `last_check_at` epoch sentinel (re-check pending) serialises as `null`, not 1970.
- SQL helpers in `gradient-db/src/project_board.rs` (grouped counts, queue summary, dependency-closure CTE) are covered end-to-end by CI; no local DB unit harness exists.

## Live evaluation progress (#295)

Build/dependency totals now grow live during evaluation: the backend was silent
while the evaluator inserted build rows (no status change), so the project and
evaluation pages stayed frozen until the eval phase finished.

- `backend/gradient-web/src/endpoints/live.rs::tests::eval_channel_matches_only_its_evaluation`
  and `project_channel_forwards_progress_and_learns_its_eval` assert the new
  `evaluation_progress` frame is forwarded on the `/evals/{id}/live` and
  `/projects/{org}/{project}/live` channels (and learnt into the project
  channel's known-eval set), while a progress ping for another evaluation /
  project is dropped.
- `backend/gradient-scheduler/src/eval.rs::handle_eval_result` emits
  `BoardEvent::EvaluationProgress` after each batch of builds/entry-points is
  persisted, so the silent insert phase no longer leaves the UI frozen.

## Substituted-build log fallback

`backend/gradient-web/src/endpoints/builds/mod.rs::effective_log_id` resolves the
build whose stored log the `/builds/{id}/log*` endpoints serve: a `Substituted`
build has no log of its own, so it falls back to the most recent prior build of
the same derivation that does (chunked or inline). Read-side, DB-dependent;
covered end-to-end by CI, no local DB unit harness.

## Entry-point dependency-closure counts (#383)

Replaces the per-request recursive closure CTE (`entry_point_dep_counts`) with an
incrementally-maintained cache: a derivation's build-time closure is materialised
once into `derivation_closure` (content-addressed, size cached on
`derivation.dep_closure_count`), and each entry point's per-status histogram is
kept in `entry_point_dep_count`, seeded at eval-completion and updated on every
build status transition.

- `backend/gradient-db/src/dep_closure.rs::tests::load_groups_rows_by_entry_point_and_status`
  and `load_returns_empty_when_no_counts_maintained` cover the read assembly and
  the empty-result signal that drives the web fallback (`MockDatabase`).
- `backend/gradient-scheduler/src/handler_tests.rs` Group E
  (`eval_job_completed_*`) gains a `seed_entry_point_dep_counts` no-entry-points
  query in each `handle_eval_job_completed` mock sequence.
- SQL-level behaviour - closure materialisation, the `apply_dep_count_delta`
  old→new shift, the delete-then-reinsert `init`, and the read-path maintained vs
  CTE-fallback branch - is DB-dependent and covered end-to-end by CI (no local
  Postgres unit harness; counts are atomic per-row, deltas are spawned, and
  restart reconciliation recomputes in-flight evals).
- The incremental deltas fire only from the single-row status hook; every bulk
  status path (`promote_ready`/`promote_dependents`/`cascade_dependency_failed`/
  `requeue_failed_anchors`) moves anchors with raw SQL that bypasses it, so the
  live histogram drifts. `check_evaluation_done` runs an authoritative
  `reconcile_eval_dep_counts` on the terminal transition (alongside the existing
  abort/startup/read-when-empty reconciles), so a settled eval's bar always
  matches its final graph regardless of which bulk paths ran. Covered E2E by CI.

## Pre-build Waiting state for missing fetch/eval workers (#381)

The scheduler parks an evaluation in `Waiting` while it cannot make progress and
names the missing capability: a `Fetching` eval needs a `fetch`-capable worker,
`Queued`/`EvaluatingFlake`/`EvaluatingDerivation` need an `eval`-capable one. The
park happens even when the eval has already batched some build rows (the previous
build-phase reconciler skipped that case), and recovers to `Queued` once the
capability returns. The reason is the `eval_workers` `WaitingReason` variant.

- `backend/gradient-types/src/waiting_reason.rs::tests::eval_workers_round_trip_carries_capability`
  - JSON round-trips the `fetch`/`eval` variants under `kind == "eval_workers"`.
- `backend/gradient-scheduler/src/build.rs` waiting-reason tests:
  - `pre_build_target_queued_no_eval_worker_stalls_to_eval_waiting` and
    `pre_build_target_fetching_no_fetch_worker_stalls_to_fetch_waiting` - the
    capability split (an eval-only pool still strands a `Fetching` eval).
  - `pre_build_target_active_pre_build_with_capability_left_alone` and
    `pre_build_target_ignores_waiting` - no-op while progressing / for `Waiting`.
  - `eval_recovery_unparks_to_queued_when_capability_returns` and
    `eval_recovery_refreshes_reason_while_capability_absent` - the `Waiting`
    recovery decision.
- `frontend/.../evaluation-log.component.spec.ts` `eval_workers waiting reason`
  block - titles/messages for the fetch, eval, and full-cache stalls.
- The full DB-driven reconcile sweep (build-phase buildability + status
  transitions) is covered end-to-end by CI (no local Postgres unit harness).

## Chunked cache upload (#390)

`gradient cache upload` splits each NAR into 32 MiB chunks so no single request
exceeds the bundled reverse proxy's 100 MiB body limit (which previously 413'd
large NARs). Chunks stage to a server-side `.partial` (reusing the `#225`
`PartialStore`) keyed by the 32-char store hash, then a finalize request
validates and ingests the staged NAR.

- `backend/gradient-web/src/endpoints/caches/upload.rs::tests` -
  `accepts_a_normal_store_basename` / `rejects_traversal_and_separators` cover the
  staging-key path-traversal guard.
- `cli/src/commands/cache_upload.rs::tests::extracts_hash_from_full_path_and_name_with_specials`
  - the URL-safe store-hash key extraction (handles `+` in store names).
- `PartialStore` staging semantics (contiguous append, offset-0 restart) are
  already covered under *Resumable NAR transfers (#225)*.
- The chunk→finalize→ingest round trip is DB- and storage-dependent and covered
  end-to-end by CI (no local Postgres unit harness).

## Project page bugs: PR label, stale packages, redirect (#391)

- `backend/gradient-forge/src/webhook.rs::tests::parse_github_pr_opened_event`
  now asserts the parsed PR `title`, which becomes the evaluation's display
  message for PR triggers (PR webhooks carry no head commit message).
- `frontend/.../project-detail.component.spec.ts`:
  - `labels a pull-request trigger as "PR #<n>"` - the trigger label shows the
    PR number (from `EvaluationSummary.pr_number`) and falls back to "PR".
  - the existing evaluation-selection specs cover the stale-packages clear on
    switch (entry points reset + reload on `select`).
- `frontend/.../evaluation-log.component.spec.ts` continues to pass with the
  `takeUntilDestroyed` teardown that stops late fetches from redirecting back to
  the log page after the user navigates away.
- The build-list latest-attempt batching, the dep-count backfill, and the
  `source_comment` PR-number persistence are DB-dependent and covered end-to-end
  by CI (no local Postgres unit harness).

## Memory-budgeted sharded evaluation (#386)

A flake's discovery used to run as one giant single-worker `discover()` over
every system at once, which on a large/many-system flake exceeds the RAM budget
and never completes. Discovery is now split into one shard per system, fanned
across a pool whose size keeps `pool_size * max_eval_rss` within a fraction of
host RAM.

- `backend/gradient-worker/src/nix/wildcard_walk.rs`:
  - `plan_*` tests assert the per-system split mirrors `walk`'s
    `*`/`#`/opaque/recover-one-level branches, and `assert_split_equivalent`
    proves the union of `discover` over the shards equals one-pass `discover` for
    trailing-`*`, non-trailing, `#`, opaque-skip, top-level and multi-include
    patterns.
  - `segments_to_pattern_quotes_dotted_segments_only` round-trips a shard's
    segments (quoting only dotted names) back through `parse_pattern`.
- `backend/gradient-worker/src/worker_pool/pool.rs::budgeted_pool_size_caps_by_memory`
  covers the no-OOM pool sizing: capped by `ram_budget / max_eval_rss`, floored
  at 1 (a tiny host still evaluates, one shard at a time), and divide-by-zero
  safe.
- The sharded `list_flake_derivations` fan-out, per-shard RSS recycle, and the
  `EVAL_RAM_SHARE`-of-host-RAM pool sizing are exercised end-to-end by the
  `gradient-eval` VM test (no local libnix harness).

### Concurrent eval-cache without WAL deadlock

Parallel shards (and concurrent evaluations of the same flake) share one
eval-cache `<fp>.sqlite`. The nix fork splits `EvalCache::commit()` (commits the
SQLite txn, appends to the WAL, no checkpoint, safe under concurrent writers)
from `EvalCache::checkpoint()` (`wal_checkpoint(passive)`, folds the WAL into the
main file without taking the exclusive read-slot lock, so it never blocks on a
concurrent reader). Gradient calls `commit_cache()` per shard and
`checkpoint_cache()` once at end-of-eval (before the fleet-share push), so
neither the per-shard `@120`-vs-`@123` deadlock nor the end-of-eval truncate
deadlock can form. The `Checkpoint` eval-op round-trips through the
`EvalRequest`/`EvalResponse` serde tests; the concurrent-write path itself is
covered by the `gradient-eval` VM test (needs the fork's libnix, no local
harness).

## SCIM provisioning (#384)

`backend/gradient-web/tests/scim.rs` drives the `/scim/v2` surface through
`axum_test::TestServer` against a `MockDatabase`, asserting SCIM-shaped
`application/scim+json` responses:

- **Auth** - a missing or wrong bearer token returns `401` with the
  `urn:ietf:params:scim:api:messages:2.0:Error` body.
- **Users CRUD** - `POST /Users` returns `201` with an `id`; duplicate `userName`
  returns `409`; `GET /Users/{id}` returns the user; `PUT`/`PATCH` update it;
  `GET /Users` filters by `userName eq "..."` and honours `startIndex`/`count`
  pagination.
- **Active toggle + delete** - `PATCH` with `active=false` deactivates;
  `DELETE /Users/{id}` soft-disables by default (issues an `UPDATE`, not a
  `DELETE`) and hard-deletes when `scim_hard_delete` is set.
- **Groups** - `PATCH /Groups/{id}` add/remove members maps to
  `organization_user` grants for the resolved `(organization, role)`; an unknown
  group name returns `404`.
- **Discovery** - `GET ServiceProviderConfig`/`ResourceTypes`/`Schemas` return the
  advertised capabilities.
- **Inactive login** - an inactive (`active=false`) user is rejected at login with
  `403`, exercised via the auth path alongside the active guard.

`gradient-types` `config.rs` unit tests cover `scim_config()` (disabled → `None`,
enabled-without-token → `None`, fully-configured → `Some`) and `RuntimeConfig`
propagation; `gradient-state` covers `resolve_scim_group_roles` mapping a
`scim_group` name to its `(org, role)` grant; `scim/filter.rs` covers the
`attr eq "value"` parser.

## Open PR flake.lock updater

The `open_pr` action opens/updates a pull request from a natively recomputed
flake.lock. An `input_update` evaluation bumps the project's tracked inputs
(override rows with `url` unset; any override with a `url` set blocks the run as
a safety gate), verifies the candidate lock by a normal eval/build per
`verify_gate`, then opens or updates the PR. v1 covers `github`, `gitlab`, and
`git` flake inputs.

Backend (`cargo test -p gradient-nix --lib lock`):
- `lock_model_round_trips` - a `flake.lock` with `github`/`gitlab`/`git` nodes
  parses into the lock model and re-serialises byte-stable, so a no-change bump
  produces an empty patch (and opens no PR).

Backend (`cargo test -p gradient-nix --lib update_input`):
- `update_input_github` / `update_input_gitlab` / `update_input_git` - per
  fetcher, the updater resolves the newest revision and rewrites the node's
  `rev`/`narHash` with a natively recomputed hash (one case per supported input
  type).

Backend (`cargo test -p gradient-flake-lock --lib`):
- `only_git_keeps_ref_in_locked` - `LockedRef::locked_keeps_ref` is true only for
  plain `git`; the github-family schemes pin by `rev` alone.
- `bumps_changed_input` / `drops_stale_ref_from_github_locked` - a bumped
  `github` node never carries a `ref` in its `locked` block (nix rejects a github
  input holding both a `rev` and a branch/tag), and an already-poisoned lock
  heals on the next bump.
- `keeps_ref_for_git_inputs` - a `git` node keeps its `ref` alongside the bumped
  `rev`, matching what nix writes for that fetcher.

Backend (`cargo test -p gradient-ci --lib actions::open_pr`):
- `matcher_fires_only_on_input_update` - the action's verify-gate matcher
  (default gate `evaluation.completed`) fires for an `input_update` evaluation and
  is a no-op for a `normal` evaluation, so ordinary runs never open a PR.
- `tracked_inputs_collected_and_pinned_override_blocks` - tracked inputs are
  collected from `url`-unset override rows, while the presence of any `url`-set
  override blocks the `input_update` run.

Backend (`cargo test -p gradient-ci --lib trigger`):
- `input_update_noop_without_open_pr_action` / `input_update_noop_without_tracked_inputs`
  - `maybe_trigger_input_update` self-gates: it creates no evaluation unless the
  project has an active `open_pr` action and at least one tracked input, so the
  shared call from the periodic dispatch and the manual *Run trigger* /
  *Start Evaluation* paths is a no-op on projects that do not qualify.
- `input_update_pinned_override_blocks_run` - a `url`-pinned override anywhere on
  the project blocks the run.
- `input_update_creates_eval_for_tracked_input` - a tracked input with no pin
  creates one `input_update` evaluation plus its sidecar.

Backend (`cargo test -p gradient-forge --lib git_push`):
- `upsert_replaces_file_and_preserves_subtree` - the force-push tree builder
  replaces `flake.lock` while preserving sibling files and nested subtrees, so a
  bump rewrites only the lock. Forges without a force-update-ref REST endpoint
  (Gitea/Forgejo, GitLab) push a single clean commit on the current base this
  way; GitHub keeps its native git-refs force-update.

Backend (`cargo test -p gradient-forge --lib reporter::pull_request`):
- `open_pr_creates_branch_and_pr` / `open_pr_updates_existing_in_place` - the
  reporter's PR methods open a fresh PR and, with `update_existing`, update an
  already-open PR in place rather than opening a duplicate.

Backend (`cargo test -p gradient-nix --test narhash_corpus`):
- `narhash_matches_nix_golden_corpus` - a golden-corpus differential test
  compares the natively recomputed `narHash` against fixtures produced by
  upstream `nix` for each fetcher, guarding the native hasher against drift.

NixOS VM (`nix/tests/gradient/open-pr`):
- E2E trigger-to-PR path: a trigger fires on a project with an `open_pr` action
  and one tracked input, the worker bumps the input and verifies the candidate
  lock, and a PR is opened on the (test) forge; a re-run with no upstream change
  produces an empty patch and opens no second PR.

## Input-update PR fires for already-built closures, despite the concurrency policy

Three independent fixes let a `flake.lock` bump actually open its PR:

1. **Gate keys off the eval transition, not a per-build event.** A bump whose
   candidate closure is already built (a reused global `derivation_build` anchor)
   or substitutable runs no fresh build, so no `build.completed` ever fires - yet
   the eval still reaches `Building`/`Completed`. The `OpenPr` gate now fires on
   `evaluation.completed` (`build` gate) / `evaluation.building` (`eval`/`none`).
   `backend/gradient-ci/src/actions/tests/mod.rs`:
   `matches_event_open_pr_fires_only_on_gate_event` asserts the `build` gate
   matches `evaluation.completed` (not `evaluation.building`, `build.completed`,
   or `evaluation.failed`) and the `eval` gate matches `evaluation.building`.

2. **The concurrent bump run is not aborted by the normal CI run.** The
   `input_update` eval is created `concurrent`; `apply_trigger` now scopes its
   in-flight lookup to non-concurrent evals (mirroring the
   `uq_evaluation_one_active_per_project` partial index, which excludes
   `concurrent` rows), so the concurrency policy and same-commit dedup ignore it.
   Covered by the partial-index parallel and E2E CI rather than a MockDatabase
   test, which cannot observe a WHERE clause.

3. **The eval's commit is blank until the PR is pushed.** `maybe_trigger_input_update`
   seeds the eval with an empty commit; the worker fetches from the sidecar's
   `base_commit`, and `point_eval_at_pushed_commit` fills the commit with the
   generated `flake.lock` commit once the branch is force-pushed.

Frontend (`**/action-form.component.spec.ts`):
- `hard-wires events to empty for open_pr` - selecting the `open_pr` type marks
  `eventsHardwired()` and submits `events: []`; the UI shows no event selector
  (the action fires on the verify gate, not user-chosen events).

Frontend (`src/app/shared/evaluation/commit.spec.ts`):
- `commitLabel` / `evaluationTitle` - a blank commit (an `input_update` eval before
  its generated `flake.lock` is pushed) renders the `[unknown]` placeholder instead
  of empty space in the project-detail strip and evaluation-log header, and falls
  back to the short hash or commit message when those are present.

## Gradient build end-to-end + nix fast paths (#422)

Fixes two blocking bugs in `gradient build` (the CLI decoded a successful blob
upload as an error; the scheduler git-cloned the materialised
`/nix/store/<hash>-source`) and adds the `nix`-feature source NAR upload and
post-build `result`.

- `backend/gradient-storage/src/source_nar.rs::tests::from_bytes_matches_dir` -
  `source_nar_from_bytes` computes the same store path/hashes as
  `materialise_source_nar` for the same NAR, so the CLI and server agree on the
  source store path.
- `backend/gradient-scheduler/src/dispatch.rs::eval_source_tests` -
  `cached_source_dispatches_without_fetch` dispatches a `/nix/store/...`
  repository as `FlakeSource::Cached` with `[EvaluateFlake, EvaluateDerivations]`
  and the source in `required_paths`; `repository_source_keeps_fetch` leaves a
  git URL on the `FlakeSource::Repository` + `FetchFlake` path.
- `backend/gradient-web/tests/build_requests_source.rs` -
  `source_upload_creates_queued_eval` (multipart NAR → Queued eval, `cache` null)
  and `source_upload_missing_nar_is_400`.
- `backend/gradient-web/tests/build_requests_dispatch.rs` - the happy paths now
  assert the `DispatchResponse.cache` field.
- `cli/connector/tests/build_requests_api.rs` - `upload_blobs_decodes_counts`
  (decodes `{uploaded,remaining}` instead of erroring on a 200) and
  `upload_source_nar_returns_dispatch`.
- The `nix copy`/`result` substitution and the staged NAR pack are daemon/`nix`
  dependent and covered end-to-end by CI.

## Slug umlauts, source NAR compression, CLI eval surfacing & fetch-by-SHA (#431, #435, #430)

- `frontend/src/app/shared/text/slug.spec.ts` - `slugify` transliterates umlauts
  (`NüschtOS` → `nuschtos`), expands `ß`→`ss`, strips other Latin diacritics, and
  keeps the lowercase/hyphen/trim behaviour. Fixes #431 (`NüschtOS` → `n-schtos`).
- `backend/gradient-storage/src/source_nar.rs::tests::compressed_bytes_round_trip_to_nar`
  - the stored `compressed_bytes` zstd-decompress back to the raw NAR, and
  `file_size`/`file_hash_sri` describe the compressed object. Fixes #435 "Zstd
  decompress failed: Unknown frame descriptor" (source NAR was stored
  uncompressed while the worker import expects `.nar.zst`).
- `cli/connector/src/evals.rs::tests` - `deserializes_eval_without_updated_at_or_error`
  and `deserializes_eval_with_error_message`: the CLI tolerates a server eval
  payload that omits `updated_at`/`error` (the decode failure behind `gradient
  watch` "Unknown evaluation" / `build` "api error (200)") and surfaces the
  populated `error`.
- #430 (fetch-by-SHA fallback for a commit not reachable from the cloned refs)
  needs a live git transport, so it is covered by CI/manual rather than a unit
  test; the worker now fetches the commit by SHA before failing with a clearer
  "not reachable (force-pushed, GC'd, or a fork PR ref)" message.

## Clickable rejected jobs on the Live Jobs board

Lets a passed-over candidate in the "incl. rejected" view open the job detail
page with its score breakdown, served from the in-memory decision ring.

- `backend/gradient-scheduler/src/jobs.rs::tests::candidates_carry_ephemeral_id_and_breakdown_for_detail_lookup`
  - every scored candidate gets an ephemeral id and per-rule `score_breakdown`;
  `JobTracker::candidate_detail(id)` reconstructs the detail (worker/instance
  context shared per decision) and returns `None` for an unknown id.
- The memory-first `GET /board/jobs/{id}` resolution (ring before DB) and the
  frontend's clickable rows + "Passed over"/"Scored" labelling are exercised
  end-to-end by the board E2E rather than a unit test.

## StorePath object + prefix-free API & server log levels (#416, #438)

- `backend/gradient-entity/src/store_path.rs::tests` - `StorePath` parses both
  full (`/nix/store/<hash>-<name>`) and bare base forms, round-trips
  `base()`/`full()`, detects `.drv` derivations, serde-serialises to the
  prefix-free base form (deserialising either form), and rejects malformed input.
- `backend/gradient-web/tests/evals_artefacts.rs` and
  `evaluation_builds_via.rs` - the public API now returns prefix-free store paths
  (`<hash>-<name>[.drv]`) for `derivation`, output `store_path`, and the build
  `name`.
- `backend/gradient-web/tests/narinfo.rs` + `caches/narinfo.rs` - the Nix
  binary-cache protocol responses (`StorePath:`/`References:`/`Deriver:`,
  `nix-cache-info` `StoreDir`) keep the `/nix/store/` prefix, reconstructed from
  the `cached_path` `hash`+`package` columns after the redundant `store_path`
  column was dropped.
- `backend/gradient-scheduler/src/views.rs::tests` - `DerivationRef.drv_path` in
  the dispatch-decision ring is normalised to the prefix-free base form.
- `backend/src/main.rs::tests` - `build_filter_directive` targets the renamed
  `gradient_*` crates (`gradient_web`/`gradient_cache`/`gradient_proto`/
  `gradient_scheduler`), bakes in dependency-noise suppression, and no longer
  emits the dead `builder=` target. Fixes #438.
- The migration (`m20260619_000001_drop_cached_path_store_path`), the CLI's
  `OutputArtefacts::full_store_path` reconstruction for `nix copy`/`nix-store
  --realise`, and the worker FFI paths are covered by CI rather than local unit
  tests.

## Direct-to-object-store NAR upload + storage/disconnect retry

Stops relaying multi-GB build-output NARs through the server (which buffered each
whole NAR and did one blocking PUT inline on the connection, freezing the worker
session on slow object storage).

- `backend/gradient-worker/src/executor/compress.rs::compress_and_push_paths` now
  routes build outputs through `CacheQuery { Push }` + the shared
  `executor::upload_one_nar`: a presigned S3 PUT straight to object storage when
  S3-backed, falling back to direct `NarPush` only for local stores. A failed
  upload propagates as a transient `BuildError`, so the build is re-dispatched.
- `backend/gradient-worker/src/executor/mod.rs` - eval-time pushes (the `.drv`
  closure and fetched flake inputs) now propagate upload failures via
  `JobReporter::push_drv_closure` returning `Result`; a failed push fails the
  evaluation with the error instead of being silently swallowed. The
  `job_reporter` fake mirrors the new signature.
- `backend/gradient-proto/src/handler/dispatch.rs::fail_build_transient` - a
  server-side staged-read / size-mismatch / `nar_storage.put` failure (local
  stores) stops the worker and marks the build `FailedTransient` so it retries,
  without dropping the WebSocket (the dispatch loop keeps running, so the
  worker's other in-flight jobs survive).
- `backend/gradient-scheduler/src/jobs.rs::worker_disconnected` now returns the
  requeued `PendingJob`s; `worker_lifecycle::unregister_worker` calls
  `build::requeue_orphaned_jobs` to reset the DB rows of a disconnected worker's
  in-flight jobs (`Building -> Queued`; mid-eval -> `Waiting`, recovered by the
  reconciler) so they re-dispatch instead of stranding. Covered by the existing
  in-memory `test_worker_disconnected_requeues` / `test_worker_disconnect_requeues_jobs`;
  the DB reset and the worker-side orphan-task cancellation are covered by CI E2E
  rather than the MockDatabase unit harness.

## Per-org multi-installation GitHub integration (#436)

GitHub is now a first-class creatable forge integration. The App remains
server-wide; installations are per-org and multiple (one per GitHub account),
created via `PUT /orgs/{org}/integrations` with `forge_type=github` and
`installation_id`, or auto-created by the install webhook.

`backend/gradient-ci/src/integration_lookup.rs` - `name_tests`:
- `uses_account_login_when_present` - `github_integration_name(Some("Acme-Corp"), 42)` yields `"github-acme-corp"` (lowercased, prefixed).
- `falls_back_to_installation_id` - `github_integration_name(None, 42)` yields `"github-42"` when no login is available.

`backend/gradient-ci/src/integration_lookup.rs` - `ensure_tests`:
- `creates_both_rows_when_none_exist` - `ensure_github_app_integrations` on an org with no existing rows inserts the inbound and outbound pair linked to the specific `github_installation`.
- `skips_kinds_that_already_exist` - repeated calls are idempotent per installation; rows that already exist for that org/installation/kind are not duplicated.

`backend/gradient-forge/src/github_app.rs` - `parses_installation_account_login`:
- `InstallationResponse` deserialises `{"id":42,"account":{"login":"acme-corp"}}` and returns `account.login = "acme-corp"`. Guards the `get_installation` API call that validates an installation id on `PUT`.

`backend/gradient-web/tests/orgs_integrations.rs`:
- `github_create_without_app_config_is_rejected` - `PUT /orgs/{org}/integrations` with `forge_type=github` returns `400` when the server has no GitHub App configured (`GRADIENT_GITHUB_APP_ID` absent). Guards the "App must be configured" prerequisite before any installation lookup.
- PATCH on a `forge_type=github` row returns `400`; github rows are installation-managed and not editable via PATCH (delete to remove). The `PatchIntegrationRequest.forge_type` enum excludes `github` at the schema level.
- github rows CAN be deleted via `DELETE /orgs/{org}/integrations/{id}`; removing the row also removes the `github_installation` binding.

`backend/gradient-web/tests/forge_hooks.rs` - reworked GitHub App dispatch tests routing by `github_installation` FK:
- `github_app_webhook_push_fires_trigger` (Test 10) - a push event dispatched via a `github_installation` row fires the matching project trigger and returns one queued evaluation.
- `github_app_webhook_installation` (Test 12) - an `installation` event for an org not found in the DB warns and returns `200` with `event="installation"` and empty queued arrays (no crash).
- `github_app_webhook_multi_org_routes_to_matching_org` (Test 15) - a push event routes only to the org whose project URL matches the push payload; the sibling org with a different repo URL is not queued.
- `github_app_webhook_no_matching_repo_returns_zero` (Test 16) - a push against a repo URL not tracked by any project returns `projects_scanned=0`.

`backend/gradient-state/src/config.rs` - `deserializes_github_integration_with_installation_id`:
- A `forge_type=github` entry in the `integrations` map deserialises with `installation_id` + optional `account_login`, covering the state provisioning path that links the `github_installation`.

`frontend/src/app/features/organizations/integrations/integrations.component.spec.ts` - `IntegrationsComponent - create github integration`:
- `createIntegration sends forge_type=github with installation_id` - form submit with `forge_type=github` and a numeric `installation_id` string calls `PUT` with the parsed integer.
- `createIntegration rejects a non-integer installation_id` - a non-numeric installation id string fails client-side validation without sending a request.

Migration backfill (`m20260620_*_github_installation_table`) and the `github_installation` FK wiring are verified by E2E CI against real PostgreSQL.

## Eval push discovers input sources by parsing the `.drv`

A real build failed `InputsUnavailable` on a `.drv`'s input source (a
`builtins.toFile` config like `grub-config.xml`) that the evaluation never
pushed: `push_drv_closure` discovered paths via the daemon's reference walk,
which does not reliably report a `.drv`'s `inputSrcs`. Input sources have no
producing derivation, so the miss could not self-heal - every re-eval re-walked
the same way and re-failed.

`backend/gradient-worker/src/executor/mod.rs` - `drv_input_sources` now parses
each produced `.drv` and unions its `inputSrcs` into the push set (mirroring the
build-side `InputPrefetcher::enumerate_inputs`), so every source a build worker
will demand is pushed by the evaluation that produced it. NAR bytes already
upload from the filesystem (`NarByteStream`), so filesystem-parsed discovery is
sufficient.

`push_drv_closure` extracts those sources from **every `.drv` in the collected
closure**, not just its seed `.drv`s. `collect_runtime_closure` already returns the
full nix-level closure (including subtrees gradient pruned during the BFS), so
parsing only the seeds left a pruned/transitive node's `inputSrcs` unpushed - and
when that node later had to rebuild (its output demoted or never fetchable), it
failed `InputsUnavailable` forever on a producerless source only the eval worker
held (observed live across a large multi-system flake: `etc-machine-id`,
`X-Restart-Triggers-polkit`, `staticPaths`, `smartd-notify.sh`, vendored
`cargo-src-*`). `drv_input_sources` parses the closure's `.drv` members
concurrently (`buffer_unordered`) since the set is now closure-sized. Covered
end-to-end in CI; the two `drv_input_sources_*` unit tests still pin the parse.

- `drv_input_sources_parses_inputsrcs_not_via_daemon` - a `.drv` fixture's two
  `grub-config.xml` `inputSrcs` are returned; its input *derivation* is not.
- `drv_input_sources_skips_unreadable_drv` - a missing `.drv` is skipped, not
  fatal (the daemon closure still covers it).

## Dispatch gate requires inputSrcs cached

Pushing a `.drv`'s `inputSrcs` is necessary but not sufficient: the readiness
gate trusted only dependency *anchors*, never the sources, so a requeued anchor
(reset to `Created` but still `edges_complete` with all deps cached) re-dispatched
the instant the periodic `promote_ready` backstop ran - before the new evaluation
re-pushed its sources - and failed `InputsUnavailable`. Under a perpetual abort
the source push never won the race and the anchor poisoned to `FailedPermanent`.

The fix records each derivation's `inputSrcs` in `derivation_input_source`
(parsed from the `.drv`, persisted at `report_eval_result` via
`persist_input_sources`, idempotent on `(derivation, hash)`) and adds a gate to
`promote_ready`, `promote_dependents`, and `dispatch_ready_builds`: a
non-substitutable anchor promotes/dispatches only when every source hash is
`fully_cached` in `cached_path`. The SQL gate is covered by mirroring across the
three queries plus E2E (no real-Postgres unit harness); the worker-side mapping
has a unit test:

- `build_discovered_derivation_carries_input_sources`
  (`backend/gradient-worker/src/executor/eval.rs`) - a parsed `Derivation` with
  two `inputSrcs` yields a `DiscoveredDerivation` carrying them, so they reach the
  server for persistence and gating.

## Substitute relay reuses upstream NARs at the level-6 window

When the worker relays a substitutable output from an upstream cache
(`relay_external_cached_outputs`, `backend/gradient-worker/src/proto/nar_import.rs`),
it stores the downloaded bytes verbatim - no decompress, no recompress, no rehash -
when they are already zstd-compressed with a window of at least 2 MiB (the window
zstd level 6 produces, `windowLog` 21), reusing the upstream `file_hash`/`nar_hash`
plumbed through `CachedPath`. Weaker windows (levels 1-2) and non-zstd payloads are
recompressed at level 6. The window is read straight from the zstd frame header:

- `zstd_window_size_decodes_window_descriptor` - `windowLog` 21 -> exactly 2 MiB,
  `windowLog` 20 -> 1 MiB, and the mantissa adds `windowBase/8` per unit.
- `zstd_window_size_rejects_non_zstd_and_truncated` - non-zstd bytes and headers
  cut off before the descriptor / window byte return `None`.
- `zstd_window_size_matches_level6_threshold` - a real level-6 frame over >2 MiB of
  data carries a window `>= LEVEL6_WINDOW_BYTES`, a level-1 frame stays below it,
  anchoring the threshold end-to-end.

The upstream `FileHash` is parsed into `CachedPath.file_hash` by
`parse_upstream_narinfo` (`backend/gradient-core/src/upstream.rs`), asserted in
`parse_upstream_narinfo_full_fields`, and persisted on `derivation_output.file_hash`
(migration `m20260625_000001`) so the relay can report it without recomputation.

## Worker liveness watchdog detects silent (OOM-killed) workers

The server otherwise learns of a departing worker only when its TCP connection
closes; a hard OOM-kill, a frozen host, or a network partition can leave the
socket half-open, so the worker stays "connected" and its in-flight eval/build
jobs sit non-terminal forever. The session loop stamps each worker's `last_seen`
on every inbound frame (the worker heartbeats every 10 s) and a watchdog
(`worker_liveness_loop`, `backend/gradient-scheduler/src/dispatch.rs`) unregisters
any worker silent past `worker_heartbeat_timeout_secs` (default 30 s), reusing
`unregister_worker` to re-queue its jobs. The deadline logic is covered in
`backend/gradient-scheduler/src/worker_pool.rs`:

- `stale_peers_flags_only_silent_workers` - a just-heard-from worker is not
  stale, a worker exactly at the deadline is not yet stale (strict `>`), one
  millisecond past it is flagged, and a freshly registered worker is stamped
  with `now` so it is never immediately stale.
- `last_seen_handle_none_for_unknown_worker` - the handle lookup returns `None`
  for a peer that is not connected.

## Free-RAM reaper caps a runaway eval before the host OOMs

`maxEvalRss` only recycles an eval subprocess *between* `list`/`resolve` calls, so
a single call can balloon past it and OOM the host first. The reaper
(`memory_reaper_loop`, `backend/gradient-worker/src/worker_pool/pool.rs`) samples
host `MemAvailable` every 500 ms and SIGKILLs the largest live eval subprocess
when free RAM falls below the margin (`min_free_ram_mb`, `0` = adaptive
`max(1 GiB, 10% of total RAM)`); the victim's parent reports the eval failed
rather than the host freezing. Covered in the same file:

- `memory_guard_bytes_configured_and_adaptive` - a configured `min_free_ram_mb`
  wins (MiB→bytes), `0` yields 10% of total RAM, and the adaptive margin floors
  at 1 GiB on small hosts.
- `pid_guard_deregisters_pid_on_drop` - the `PidGuard` RAII field removes a
  subprocess pid from the pool's live registry on drop, so the reaper never
  targets a worker that has already been discarded.

## Upstream latency ordering (#464)

- `gradient-db` `UpstreamAccum`: `accum_record_hit_counts_hit_and_latency`,
  `accum_record_miss_counts_miss_and_latency`, `accum_record_error_counts_latency_only`.
- `gradient-core` ordering/selection: `order_endpoints_hit_rate_desc_then_latency_asc`,
  `order_endpoints_unknown_hit_rate_sorts_last`, `should_race_only_for_small_n`,
  `select_best_hit_picks_lowest_latency_hit`, `select_best_hit_none_when_all_miss`,
  `fold_samples_aggregates_per_upstream`.
- `parse_upstream_narinfo` parser tests remain (unchanged).
- Frontend: `cache.component.spec.ts` renders an upstream row from `getUpstreams`.
- SQL coverage (window stats query, per-batch upsert, rollup inserts) is by
  mirrored-query review plus CI E2E; no real-Postgres unit harness exists.
- Org-scoping of `/board/cache/upstreams` mirrors `get_board_network` (MetricsScope) and is covered by CI E2E (no local real-Postgres harness).

## Standalone evaluator crate + `gradient eval` (#472)

The worker's flake evaluator (`wildcard_walk`, `flake_walk`, `nix_eval`,
`eval_worker`, stats) moved into a self-contained `backend/gradient-eval` crate
so the CLI can reuse it. Their existing unit tests moved with them
(`wildcard_walk::tests::*` discovery/sharding, `eval_worker::tests::*` serde
round-trips); the worker keeps the pool-internal `eval_stats` accumulator tests.

- `gradient-eval` `jobs::tests::success_job_serializes_like_nix_eval_jobs` - a
  resolved `Job` serializes to `{attr, attrPath, drvPath}` with the full
  `/nix/store` path and omits empty `references`.
- `jobs::tests::failed_job_serializes_error_without_drv_path` - a failed `Job`
  serializes `{attr, error}` and no `drvPath`, matching nix-eval-jobs' per-attr
  error lines.
- `cli` `tests/eval.rs` (feature-gated on `eval`):
  `eval_help_describes_nix_eval_jobs_like_output` and `eval_requires_a_pattern`
  cover the subcommand surface.
