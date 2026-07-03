# AUDIT-DB.md - Database Entities & Schema

Scope: `backend/gradient-entity` (68 entity modules, ~4.1k LOC) and `backend/gradient-migration` (170 ordered migrations, ~13k LOC). PostgreSQL via sea-orm. File:line references are against `main` at the time of audit. Produced by a multi-agent code audit.

Related: schema flags and status enums are the substrate for the scheduling correctness issues in AUDIT-SCHEDULER.md; the raw-SQL magic-int literals discussed there originate here.

---

## Schema overview - core build-graph model

The build graph is split into a content-addressed "build-once" layer (durable, deduplicated per derivation) and a per-evaluation layer (attribution + scoring). This split landed in the 2026-06-19 globalization rework (`m20260619_010000_globalize_derivation`, `..._020000_derivation_build_anchor`, `..._030000_build_job_and_attempt`) and replaced the old monolithic `build` table.

```
                     content-addressed "build-once" layer
  +--------------------------------------------------------------------+
  |  derivation (1)  -- hash+name UNIQUE (idx-derivation-hash-name)     |
  |  entity: derivation.rs                                              |
  |     ^   ^   ^                                                       |
  |     |   |   +-------- derivation_output (N)   [outputs: name,hash,ca|
  |     |   |              derivation_output.rs   nar_*, file_*, is_cached
  |     |   |              cached_path FK --------------+  external_url] |
  |     |   |                                          v                |
  |     |   +-------- derivation_dependency (N)   cached_path (1 per    |
  |     |   |          (direct build edges)        hash, UNIQUE)        |
  |     |   |          derivation_dependency.rs    cached_path.rs       |
  |     |   +-------- derivation_closure (N)       [narinfo metadata +  |
  |     |   |          (materialized transitive     closure_complete]   |
  |     |   |          closure) derivation_closure.rs                   |
  |     |   +-------- derivation_input_source (N) [inputSrcs, no producer]
  |     |   +-------- cache_derivation (N)  [per-cache closure presence] |
  |     |                                                               |
  |  derivation_build (1, UNIQUE on derivation)  <-- THE build-once anchor
  |  derivation_build.rs   [status, substitutable, substituted,        |
  |                         edges_complete, edges_unresolved,           |
  |                         closure_complete, drv_closure_cached, attempt]
  +--------------------------------------------------------------------+
                     ^                              ^
        per-eval     | derivation_build FK          | derivation_build FK
        layer   +----+----------+          +--------+---------+
                | build_job     |<---------| build_attempt    |
                | build_job.rs  | build_job| build_attempt.rs |
                | UNIQUE        |  FK      | [outcome, reason,|
                | (evaluation,  |          |  dispatched_job] |
                |  derivation)  |          +------------------+
                | [score]       |                  | dispatched_job FK
                +-------+-------+                   v
                        | evaluation FK      dispatched_job (telemetry)
                        v
                   evaluation -- commit, project(nullable), previous/next
                   evaluation.rs   (self-referential linked list)
                        |
                        v
                   entry_point (project x evaluation x derivation = a CI check)
```

Anchors (global, build-once): `derivation`, `derivation_output`, `derivation_dependency`, `derivation_closure`, `derivation_input_source`, `derivation_build`, `cached_path`. One row per content hash, reused across every eval that needs it. `derivation_build` (UNIQUE on `derivation`, `derivation_build.rs:25`) is the single source of truth for "has this been built."

Per-eval rows: `build_job` (UNIQUE `(evaluation, derivation)`, carries the dispatch score), `build_attempt` (one per dispatch), `dispatched_job` (scoring/telemetry ring), `evaluation`, `entry_point`.

The authoritative build state (`status`) lives only on the global anchor; `build_job` is pure attribution. Promotion (`Created -> Queued -> Building -> terminal`) is graph-driven in `gradient-db/src/promotion.rs`, decoupled from eval completion.

---

## Entity/schema design smells

Ranked by impact.

**1. Status enums stored as bare `Integer`, driving 43 hand-written magic-int SQL literals (highest impact).**
`BuildStatus` (`build.rs:31`, `db_type = "Integer"`) and `EvaluationStatus` (`evaluation.rs:29`) are `DeriveActiveEnum` over `i32` with no DB-level typing. The scheduler/db layer then hard-codes those integers in raw SQL: 43 occurrences across `gradient-db/src/promotion.rs`, `rollup.rs`, `cache_storage.rs`, `project_board.rs`, and `gradient-web/src/endpoints/{metrics,board,board_metrics}.rs`. Examples: `promotion.rs:51` `SET status = 6`, `promotion.rs:87` `dep.status IN (3, 7)`, `promotion.rs:58` `dep.status IN (4, 6, 9)`. The mapping is duplicated as a hand-maintained comment at `promotion.rs:22-23`. Fragility is not hypothetical: it is inconsistent even internally, ~12 sites correctly bind `i32::from(BuildStatus::Completed)` (`gradient-scheduler/src/dispatch.rs:1074-1076`, `eval.rs:417-447`, `gradient-db/src/recovery.rs:135-138`) while the promotion hot path hard-codes literals. A single renumber breaks half of them silently. The smoking gun is `m20260407_000000_renumber_evaluation_status`, an in-place high-to-low `UPDATE` that shifted every status int (Aborted 5->7, Failed 4->6) precisely because the numbers are load-bearing.

**2. Untyped `i16`/`i32` discriminants with only a doc-comment "enum".**
No Rust enum at all, just a magic int and a prose legend:
- `integration.rs:23` `kind: i16` (0 = inbound, 1 = outbound) and `integration.rs:25` `forge_type: i16` (0 = gitea, 1 = forgejo, 2 = gitlab, 3 = github).
- `project.rs:35` `concurrency: i16` (0 = hard_abort, 1 = soft_abort, 2 = allow, 3 = skip).
- `dispatched_job.rs:18` `kind: i16`; `phase_event.rs:19` `subject_kind: i16`.
- `project_action.rs:20-21` (ActionType: 0=webhook, 1=email), `project_trigger.rs:19` (0=polling..3=time), `worker_sample.rs:28` `state: i16`, `metric_rollup.rs:19` `granularity: i16`.

These are strictly worse than #1: there is no typed round-trip anywhere, so callers pattern-match raw integers. Compare against the codebase's own good pattern (`CacheUpstreamKind` in `cache_upstream.rs:12-22`, `AttemptOutcome`/`AttemptFailureReason` in `build_attempt.rs:18-55`, `EvaluationKind`/`EvalCacheStatus` in `evaluation.rs`).

**3. Denormalized derived flags maintained by application code with no DB enforcement.**
These booleans encode graph-wide invariants as per-row state that must be reconciled:
- `derivation_build.rs:34` `edges_complete`, `:41` `edges_unresolved`, `:46` `closure_complete`, `:54` `drv_closure_cached`.
- `derivation_output.rs:24` `is_cached` (+ paired `cached_path` FK, see `CacheLink` at `derivation_output.rs:71-87`).
- `cached_path.rs:40` `closure_complete`.

Schema-level fragility: each flag is a cached projection of a query over other tables (e.g. `closure_complete` iff "all outputs in `cached_path` with `file_hash` NOT NULL AND all deps `closure_complete OR substitutable`", inlined as `CLOSURE_COMPLETE_GATE` at `promotion.rs:110-122`). Nothing in the schema (no CHECK, trigger, generated column, or FK) keeps the flag consistent with ground truth. The entire recent bug history is stale-flag dead zones (`closure_complete` stale-true, `drv_closure_cached` stale-true, `edges_unresolved` sticky-prune, unbacked-`is_cached`), all "monotonic set-only reconcile drifted from reality after GC." This is the single most dangerous structural pattern: it trades a bounded graph walk for O(1) reads at the cost of unbounded correctness liability. See AUDIT-SCHEDULER.md for the reconciliation machinery this forces.

**4. NAR-metadata duplicated across `derivation_output` and `cached_path`.**
The narinfo fields exist on both tables: `derivation_output.rs:22-39` (`ca`, `nar_size`, `nar_hash`, `file_hash`, `file_size`, `references`, `deriver`) vs `cached_path.rs:30-44` (`file_hash`, `file_size`, `nar_size`, `nar_hash`, `ca`, `deriver`). `derivation_output.cached_path` is already an FK to the row that authoritatively holds this metadata, so most of `derivation_output`'s copy is redundant (kept partly for the upstream-resolution case where `external_url` is set but no `cached_path` row exists yet, `derivation_output.rs:26-34`). Separately, `prefer_local_build` is duplicated: `derivation.rs:22` and `derivation_build.rs:58`.

**5. Nullable sprawl, heaviest on `evaluation`.**
`evaluation.rs` has 14 `Option<>` of 24 columns (`project`, `previous`, `next`, `flake_source`, `check_run_ids`, `waiting_reason`, `trigger`, `started_by`, `source_comment`, and five phase-timestamp `Option<NaiveDateTime>`). `derivation_output` 9/16, `cached_path` 6/11. Mixed semantics: some nulls mean "not yet" (phase timings), some "N/A" (`project` for direct builds). No partial indexes or CHECKs distinguish them.

**6. `Json` vs `serde_json::Value` used interchangeably, plus untyped blobs.**
Same logical type, two Rust spellings across the crate: `Json` at `build_job.rs:28`, `dispatched_job.rs:28-32`, `worker_connection.rs:23`, `metric_rollup.rs:21`, `evaluation.rs:144`; `serde_json::Value` at `evaluation.rs:145,149`, `project_action.rs:22-23`, `project_trigger.rs:21`, `upload_session.rs:23-24`, `evaluation_input_update.rs:28,33`. `evaluation` itself mixes both. Several are schemaless dumps with no typed accessor (`build_context`, `worker_context`, `job_context`, `candidates`, `score_breakdown`).

**7. Typed-id / `StorePath` discipline breaks in a few places.**
The crate is otherwise rigorous (every PK is a newtype via `ids.rs`, and `StorePath` (`store_path.rs`) is the canonical path type). Exceptions: `phase_event.rs:20` `subject_id: Uuid` (raw, polymorphic FK with no type), `dispatched_job.rs:22` `worker_id: String`, `evaluation.rs:133` `repository: String`. Store paths stored as raw `String` instead of `StorePath`: `derivation_output.rs:29,38,39` (`external_url`, `references`, `deriver`), `cached_path.rs:44` `deriver`, `derivation_input_source.rs:25` `store_path`.

**8. `Architecture` is a free-form `String`; `derivation` uniqueness omits it.**
`server.rs:8` `pub type Architecture = String`, no enum/validation for `"x86_64-linux"` etc. (converted from an enum in `m20260412_000002_convert_architecture_to_string`). The DB unique index is `(hash, name)` only (`m20260619_010000_globalize_derivation:100`), relying on Nix hashing arch into `hash`, correct, but the `architecture` column is then pure denormalized redundancy on `derivation.rs:20`.

**9. Relation coverage is thin / inconsistently authored.**
`derivation.rs:50` declares `Relation {}`, empty. There are no reverse relations from `derivation` to its outputs/deps/build/anchor, so all traversal is hand-written SQL. Relation authoring style is split: most use the `DeriveRelation` attribute macro, but `derivation_dependency.rs:27-40`, `derivation_closure.rs:32-45`, and `cache_upstream.rs:46-55` hand-implement `RelationTrait` (needed for two FKs to the same table), and `build_product.rs:39-43` adds a manual `Related` impl on top of the derive.

**10. Overlapping / candidate-dead entities.**
`derivation_closure` (materialized transitive closure) overlaps `derivation_dependency` (direct edges): the former is a denormalized cache of the latter's transitive hull, with the same drift risk as #3. `cache_derivation` (per-cache closure presence, `cache_derivation.rs:13-18`) overlaps the `closure_complete` flags. Telemetry is fragmented across seven near-parallel tables: `cache_metric`, `upstream_metric`, `derivation_metric`, `evaluation_metric`, `evaluation_attr_cost`, `metric_rollup`, `worker_sample`, `phase_event`.

---

## Migration sprawl

170 migrations, verb histogram: 53 `add_*`, 43 `create_table_*`, 17 other `create_*`, 10 `drop_*`, 4 `rename_*`, 4 `index_*`, plus one-off `renumber/normalize/globalize/slim/backfill/split/seed/hash/strip/move`.

**Add-then-churn-then-drop lifecycles (dead weight).** Several tables were built up column-by-column, then wholesale replaced or dropped:
- The `build` table saga: `create_table_build` (2024-11) then 8 add-migrations (`add_log_id`/`add_build_time_ms`/`add_via`/`add_external_cached`/`add_phase_timing`/`add_queued_at`/`add_substituted`/`add_build_failure_retry_fields`) then `split_build_into_derivation` (drops the table in `up()`, `m20260408_000000:37`) then `slim_build_and_dispatched_job` then `globalize_derivation`/`derivation_build_anchor`/`build_job_and_attempt`. The entire pre-2026-06-19 build model is now dead schema still described by 20+ migrations.
- `direct_build`: created `m20250705_000000` then dropped `m20260519_000004` (~10 months, never survived).
- `webhook` + `webhook_delivery` + `project_integration`: created 2026-03/04 then all dropped 2026-05-24 (`m20260524_000002/003/004`), replaced by `project_action`.
- `acknowledged_derivation`: created `m20260607_000005` then dropped `m20260626_000002` (lived 19 days).
- `derivation_output` file columns: `add_nar_size` (`m20260407_000001`) then `normalize_hash_columns` (`m20260430`) then `drop_file_columns_from_derivation_output` (`m20260502_000001`) then re-added as `derivation_output_file_hash` (`m20260625_000001`). Added, dropped, re-added.
- GitHub install: single `organization.github_installation_id` column then dedicated `github_installation` table + `backfill_drop_org_installation` (`m20260620_000002`).

**Data migrations interleaved with schema (~15).** These mutate rows, not just DDL: `renumber_evaluation_status` (in-place status shuffle, the direct consequence of smell #1), `globalize_derivation` (dedup rows by `(hash,name)`, re-point every FK, rebuild unique indexes, 100+ LOC of runtime SQL), `backfill_build_attempt`, `seed_github_app_integrations`, `hash_api_keys`, `normalize_hash_columns`, `normalize_derivation_columns`, `strip_derivation_path_prefix`, `convert_architecture_to_string`, `move_concurrency_to_project`, `tag_waiting_reason_kind`. Mixing lossy data transforms into the same ordered stream as DDL means the migration set cannot be replayed against a fresh DB and a prod DB with identical guarantees.

**Irreversible / risky `down()`s.** `globalize_derivation` explicitly returns an error from `down()` (`m20260619_010000:112-114`, "is irreversible"). `split_build_into_derivation` `down()` is a silent no-op (`m20260408_000000:432-436`). Most data migrations are lossy-on-reverse. There is no consistent policy: some migrations implement real `down()`, others no-op, one errors.

**What's healthy.** Naming is disciplined: uniform `m<YYYYMMDD>_<NNNNNN>_<verb>_<subject>` with intra-day ordering via the 6-digit suffix. No filename collisions. Recent flag-adding migrations (`closure_complete`, `edges_complete`, `edges_unresolved`, `drv_closure_cached`) are small and focused.

**Squash/baseline is warranted.** Roughly the first ~120 migrations (2024-11 to the 2026-06-19 globalization boundary) describe schema that no longer exists in its historical shape. A squashed baseline that emits the current schema as one `create_*` migration, with the post-globalization migrations layered on top, would delete the majority of dead DDL and make a from-scratch bring-up fast and auditable. The cost is coordinating the already-applied history on prod (baseline must be a no-op against existing DBs, e.g. via a version-gate or `IF NOT EXISTS` guard).

---

## Refactoring recommendations

Ranked by impact-to-effort.

**1. Kill the magic-int SQL literals (addresses #1).** Two viable paths, senior-preferred is the first:
- PostgreSQL native enum types. `CREATE TYPE build_status AS ENUM (...)` and switch `BuildStatus`/`EvaluationStatus` to `db_type = "Enum"`. The DB then rejects invalid values, and a renumber becomes impossible-by-construction (labels, not ordinals). Requires a data migration and updating the raw SQL to compare against labels.
- Minimum viable: eliminate every hard-coded literal in `promotion.rs`/`rollup.rs`/`cache_storage.rs`/`project_board.rs`/web endpoints by binding `i32::from(BuildStatus::X)` (the pattern already used in `dispatch.rs:1074`) or defining `const` SQL fragments generated from the enum, and delete the hand-maintained legend at `promotion.rs:22-23`. Add a compile-time test asserting each `num_value` so a reorder fails CI.

**2. Promote the doc-comment discriminants to `DeriveActiveEnum` (addresses #2).** Give `integration.kind`/`forge_type`, `project.concurrency`, `dispatched_job.kind`, `phase_event.subject_kind`, `project_action` type, `project_trigger` type, `worker_sample.state`, `metric_rollup.granularity` real enums, mirroring `AttemptFailureReason` (`build_attempt.rs:36-55`). Low risk (values already stable), high clarity win, removes ~8 prose legends that can silently rot.

**3. Draw an explicit "derived vs authoritative" boundary for the reconciled flags (addresses #3, the correctness hotspot).** Options in descending rigor:
- Replace the cached booleans with generated columns / a materialized view computed from ground truth (`cached_path.file_hash IS NOT NULL`, dependency status), so drift is structurally impossible. `closure_complete`/`drv_closure_cached` are exactly the projections already spelled out as `CLOSURE_COMPLETE_GATE`.
- If the O(1)-read performance is required, keep the flags but (a) centralize all mutation in one reconcile module, (b) make every reconcile bidirectional (CLEAR-then-SET fixpoint) as already done for `closure_complete`/`drv_closure_cached`, and (c) add a CI/periodic invariant assertion query that flags any row where the cached boolean disagrees with the ground-truth query. See AUDIT-SCHEDULER.md rec. on a single graph reconciler.

**4. De-duplicate NAR metadata and `prefer_local_build` (addresses #4).** Make `cached_path` the single narinfo source of truth; reduce `derivation_output` to `{derivation, name, hash, cached_path FK, is_cached, external_url + the upstream-only resolution fields}` and drop its redundant `nar_size/nar_hash/file_hash/file_size/ca/deriver` once every reader goes through the FK. Drop `derivation.prefer_local_build` (keep the anchor's copy) or vice-versa.

**5. Finish the typed-id / `StorePath` conversion (addresses #7).** `phase_event.subject_id` to an enum-tagged typed id or split columns; `dispatched_job.worker_id` to a `WorkerId` newtype; store-path `String` columns (`external_url`, `references`, `deriver`, `derivation_input_source.store_path`) to `StorePath`. Standardize on `Json` (drop bare `serde_json::Value`) and give the schemaless blobs typed wrappers where read.

**6. Constraints & indexes.** Verify/add UNIQUE on `derivation_dependency (derivation, dependency)` and `derivation_closure (root, dep)` (partially added in `m20260613_000001`) to prevent duplicate edges feeding the reconcile queries. Add partial indexes matching the dispatch gates (e.g. `derivation_build (derivation) WHERE status IN (0,1) AND edges_complete`), the promotion queries at `promotion.rs:52,73` scan on exactly these predicates. Reflect the DB-level `(hash,name)` unique in the `derivation` entity annotation for accuracy.

**7. Baseline/squash the migration history.** Introduce a single guarded baseline migration emitting the current schema, gate it to no-op on already-migrated DBs, and prune the pre-globalization DDL/data migrations from source. Adopt a uniform `down()` policy: real inverse where cheap, explicit `Err("irreversible")` (like `globalize_derivation`) everywhere lossy, no silent no-ops (`split_build_into_derivation:432`).

**Key files:** `backend/gradient-entity/src/build.rs:31`, `evaluation.rs:29`, `derivation_build.rs:34-58`, `derivation_output.rs:22-39`, `cached_path.rs:30-44`, `integration.rs:23-25`, `project.rs:35`, `phase_event.rs:19-20`; `backend/gradient-db/src/promotion.rs:22-122`; migrations `m20260407_000000_renumber_evaluation_status.rs`, `m20260408_000000_split_build_into_derivation.rs`, `m20260619_010000_globalize_derivation.rs`.
