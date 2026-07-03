# AUDIT-TEST.md - Test Suite

Scope: the whole backend test suite (~1,896 tracked test functions across 23 crates) plus `gradient-test-support`. Findings were verified first-hand against individual test bodies, judged against an explicit rubric. Produced by a multi-agent code audit. File:line references are against `main` at audit time.

The suite is large and disciplined about coverage but not about cost. Almost every regression class has a test, yet the tests are written one-per-case with copy-pasted setup, hand-rolled async runtimes, and assertions welded to internal query order. The result is roughly 1,900 test functions where a well-factored suite would need a few hundred, and a refactor that preserves behavior still breaks dozens of tests.

## Current shape

Real test functions (tracked files, `target/` excluded; the raw grep figure of ~3,648 was inflated by generated artifacts such as `target/.../typenum-*/out/tests.rs` with 1,743 `#[test]` alone):

| Crate | Tests | Crate | Tests |
|---|---|---|---|
| gradient-web | 443 | gradient-ci | 82 |
| gradient-worker | 187 | gradient-sources | 55 |
| gradient-proto | 150 | gradient-score | 52 |
| gradient-scheduler | 145 | gradient-util | 46 |
| gradient-db | 130 | gradient-entity | 36 |
| gradient-types | 124 | gradient-storage | 33 |
| gradient-forge | 115 | gradient-eval | 30 |
| gradient-core | 114 | gradient-cache | 24 |
| gradient-state | 86 | gradient-nix / flake-lock / test-support | 15 / 11 / 11 |

Total: ~1,896 test fns.

Cross-cutting counts (tracked):
- `MockDatabase::append_query_results` calls: 983 total (gradient-web 660, gradient-ci 167, gradient-test-support 54, gradient-db 49, gradient-scheduler 22).
- Manual async-runtime wrappers: 192 `.block_on(` calls across 47 files that build a `tokio::runtime::Builder::new_current_thread()` by hand instead of `#[tokio::test]`.
- Parameterization libraries in any `Cargo.toml`: zero (no `rstest`, `proptest`, or `test-case`). `axum-test` 20.0 (`backend/Cargo.toml:164`), sea-orm `mock` feature, and `tokio-test` are available; `tokio-test` is present but unused for the wrappers above.

What `gradient-test-support` already offers (and is under-used):
- `web.rs:89-144`: `make_test_server`, `make_test_server_with`, `make_test_server_configured`, and the private `server_from_cli` that builds the whole `ServerState` + `axum_test::TestServer` once.
- `state.rs:28-117`: `test_state`, `test_state_cache`, `test_state_with_log_storage` (three near-identical ~30-line `ServerState` literals).
- `web.rs:52-80`: `make_token` + `live_session` for the auth middleware.
- `fixtures.rs:21-185`: deterministic `org()`, `user()`, `superuser_user()`, `org_with_id`, `cache_with_id`, `eval_at`, stable UUIDs.

Adoption gap: only 19 of 47 `gradient-web/tests/*.rs` import `make_test_server`; 17 re-implement a full `ServerState { .. }` literal in-file, 17 define a bespoke `with_auth` DB mock chain, and 7 redefine `project_row()` locally.

## Rubric

Each inspected test was judged against ten rules:
1. Behavior over implementation - assert observable contract (status, return, persisted state), not query order or private call sequence.
2. One reason to fail - one behavior per test; value-only variants collapse into a table.
3. Minimal assertions - assert the field under test, not whole bodies / exact error sentences / incidental fields.
4. No duplicated setup - shared builders, not copy-pasted seed blocks.
5. Don't test the framework - no hand-rolled serde/rkyv round-trips per variant.
6. Deterministic and isolated - inject time/randomness; no cross-test state.
7. Clear intent and naming - arrange/act/assert reads cleanly; no boilerplate that obscures.
8. Right altitude - prefer one real-router integration test + a few focused unit tests over many mock-chain tests.
9. Use shared infrastructure - `#[tokio::test]`, `make_test_server`, a value-table lib.
10. Coverage value - catches a real regression class; prune near-duplicates.

## Findings by rule

**Rule 1 - behavior over implementation (widespread, highest severity).** The 983 `append_query_results` chains assert the exact internal DB query sequence, so a refactor that keeps behavior identical (reorder a lookup, add a cache) breaks the test. The canonical offender is `gradient-web/tests/forge_hooks.rs:383-404`, `apply_trigger_db_chain`, which pins 14 queries in order with a comment per row:

```rust
db.append_query_results([Vec::<evaluation::Model>::new()])   // in-flight check
  .append_query_results([Vec::<evaluation::Model>::new()])   // trigger_evaluation: in-progress check
  .append_query_results([vec![commit_row()]])                // INSERT commit
  .append_query_results([vec![eval_row(EvaluationStatus::Queued)]]) // INSERT eval
  ... 10 more, each a comment documenting an internal query
```

Same shape in `triggers.rs:123-154` (`with_auth` / `with_project_edit` encode the exact session, session, user, org, project, membership, role SELECT order, spelled out in the file header at `triggers.rs:13-21`); in `gradient-scheduler/src/dispatch_tests.rs:102-111` (per-query comments plus a call-sequence schema at header `:12-23`); and in `gradient-ci/src/apply/tests/mod.rs` where `hard_abort_populates_aborted_fields` (~`:358-404`) chains 15+ ordered results. These are unit tests of query ordering wearing an integration-test costume: they mount the real router but mock the DB so tightly they verify plumbing, not persisted outcomes.

**Rule 2 - one reason to fail / collapse value-variants (widespread).**
- `gradient-proto/src/tests.rs`: 34 of 38 functions are `*_roundtrip` differing only in the value constructed (`init_connection_roundtrip:16`, `assign_job_roundtrip:122`, `cache_status_roundtrip:207`, the nine `eval_cache_*_roundtrip` at `:433-567`). One value list replaces all 34.
- `gradient-state/src/tests/mod.rs`: value-only clusters - concurrency default vs `hard_abort` (`:84` vs `:540`), `wildcard` vs legacy `evaluation_wildcard` alias (`:101` vs `:119`), `keep_evaluations` default vs zero-rejected (`:139` vs `:156`), org-member role accept/reject (`:757-864`, 5 tests), org-id accept/default/malformed/duplicate (`:624-701`, 4 tests).
- `gradient-scheduler/src/build.rs:1555-1643`: 12 `retry_tests` all calling `decide_failure_outcome(kind, attempt, budget)`; `jobs.rs:1024-1280`: the `WorkerCaps` capability matrix as ~8 separate fns.
- `gradient-ci/src/trigger/tests/mod.rs:103-126`: `trigger_each_active_status_blocks` already loops over five statuses in one test - right instinct, hand-rolled instead of a case table.

**Rule 3 - minimal assertions / no exact error sentences (moderate).** 27 web assertions pin an exact human-readable message: `forge_hooks.rs:599` "invalid webhook signature", `:637` "integration not found", `:1128` "github app integration not configured", `cli_device_authorization.rs:251`. Prefer status + stable error code + substring. CI apply tests re-pin exact input values as if outputs (`gradient-ci/src/apply/tests/mod.rs` asserts `pr_number == 42`, `pr_author == "external-contrib"`, the literals it fed in). Counter-example to keep: `gradient-state/src/tests/mod.rs:189-196` asserts `e.field == "projects.web.keep_evaluations" && e.message.contains("at least 1")`, field code + substring is the resilient shape.

**Rule 4 - duplicated setup (widespread, highest compaction value).** Shared builders exist but are bypassed: 17 files re-roll a ~30-line `ServerState { .. }` literal (`forge_hooks.rs:57-96` `make_state`, `scim.rs:51-91` `build_server` whose own comment `:37-38` admits it "Mirrors the ServerState field set from tests/auth_middleware.rs", plus `actions.rs`, `metrics.rs`, `narinfo.rs`, `rate_limit.rs`); 17 files define their own `with_auth` chain (`triggers.rs:127-132`); 7 files redefine `project_row()` (`forge_hooks.rs:233`, `triggers.rs:37`); `forge_hooks.rs:100-346` rebuilds an entire parallel fixture layer alongside `fixtures.rs`. Even the shared crate duplicates itself: `state.rs:28-117` is three copies of one literal. `gradient-db` re-instantiates near-identical mock chains per test (`cache_reach.rs:128-312`, `recovery.rs:176-221`).

**Rule 5 - testing the framework.** The 34 proto round-trips exercise rkyv's derive. A single generic `assert_roundtrip::<T>(value)` plus a few representative values proves the same thing. Scattered one-offs elsewhere (`gradient-worker/src/proto/nar_import.rs` x3, `gradient-eval/src/eval_worker.rs` x2) are minor.

**Rule 6 - determinism.** Mostly fine; time/randomness is ambient in a few spots (`web.rs:53` `make_token` uses `Utc::now()`; many tests mint `now_v7()` IDs). Not flaky today; low severity.

**Rule 7 - boilerplate that obscures intent (widespread, built on a false premise).** 47 files wrap every async test in `tokio::runtime::Builder::new_current_thread()...block_on(...)`, justified by a comment repeated verbatim in 7 files (`forge_hooks.rs:15-16`, `triggers.rs:10-11`, `body_size_limit.rs:15-16`): "`#[tokio::test]` expands to `::gradient_core::...` which clashes with the local `core` crate name." This is stale and false: no crate is named `core`, and `#[tokio::test]` is used successfully in `gradient-web/tests/oidc_pkce.rs` and `src/endpoints/{builds/closure.rs,projects/metrics.rs}`. Five of the seven files carrying the comment never even use the attribute they claim is broken. Delete the wrappers wholesale.

**Rule 8 - altitude.** Web tests run the real `create_router` (good) but pair it with ordered mock chains, negating the integration value while still paying integration setup cost. High-value auth/trigger paths would be better served by a few real-DB tests plus focused unit tests.

**Rule 10 - coverage value.** The proto round-trips and state value-variants are near-duplicates with low marginal value; collapsing loses no coverage.

**The model to emulate: `gradient-score`.** `gradient-score/src/rules/fair_share.rs:87-143` is the target style: tiny shared arrange helpers (`build_job`/`ctx`/`worker` at `:58-85`), relative behavioral assertions (`assert!(busy < quiet, ...)` `:94`; `assert_eq!(rule.score(&busy, &w, &spare), 0.0, "idle workers must not be left empty...")` `:111-115`), names stating behavior + condition. Also `gradient-scheduler/src/scheduler_tests.rs:93-108` and `gradient-db/src/cache_reach.rs:259-311`.

## Generalization plan

**A. Shared server builder + auth helper (biggest duplication win).** Replace the 17 in-file `make_state`/`build_server` copies (`forge_hooks.rs:57-96`, `scim.rs:51-91`) with one `TestServerBuilder` in `gradient-test-support/src/web.rs` (fluent `.web_db()/.cache_db()/.configure()/.build()`), plus a shared `with_session(db, sid)` for the 3-row auth append. Removes ~510 lines of `ServerState` literals, ~17 `with_auth` copies, the 7 `project_row` copies, and `forge_hooks.rs:100-346`'s parallel fixtures; folds `state.rs`'s three variants into one (~120 lines).

**B. `rstest` for value-only variants.** Add `rstest` to workspace dev-deps; convert `gradient-scheduler/src/build.rs:1555-1643` (12 `decide_failure_outcome` fns) into one `#[case]` table, the 34 proto round-trips into one generic `assert_roundtrip<T>` + ~8 representative values, and the `gradient-state` clusters onto the existing `reporter_cfg`/`integration_cfg`/`worker_cfg` builders (28 of 40 tests bypass them for inline JSON today).

Estimated reduction: ~1,500-2,000 lines and ~250-350 test functions collapsed, with no loss of asserted behavior.

## Prioritized recommendations

1. Delete the manual-runtime boilerplate first (mechanical, zero-risk): remove the stale "core clash" comments, switch all 47 files / 192 `block_on` wrappers to `#[tokio::test]` (~600 lines).
2. Land one `TestServerBuilder` + `with_session`, reuse `fixtures.rs`, then migrate the 17 bespoke-`ServerState` / 17 `with_auth` / 7 `project_row` files onto it and delete the parallel fixtures; consolidate `state.rs`'s three copies.
3. Adopt `rstest` and convert the value-only clusters (proto, state serde, scheduler retry/caps, ci trigger-status loop).
4. Loosen implementation-coupled assertions: exact sentences (27 sites) to status + code + substring; move the highest-value auth/trigger/apply paths off ordered mock chains onto a seeded real DB so they survive query refactors.
5. Make `gradient-score` the house style: relative/behavioral assertions, shared arrange helpers, rationale-bearing messages.

Guardrail (add to `CLAUDE.md` and `docs/src/tests.md`). New backend tests MUST use `gradient-test-support` builders + `#[tokio::test]` (no per-file `ServerState` literals, no `new_current_thread` wrappers), assert outcomes not query order or exact sentences, use `rstest` tables for value variants, and avoid `append_query_results` chains deeper than ~3 without justification. A tiny CI check can reject new `new_current_thread().block_on` in tests and new in-file `ServerState {` outside `gradient-test-support` so the duplication cannot regrow.
