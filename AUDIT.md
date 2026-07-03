# AUDIT.md - Gradient Backend Code Audit (index)

A structured audit of the Gradient CI backend (Rust Nix build farm), focused on the scheduling subsystem and other areas that have grown messy and should be refactored. Findings were gathered by parallel agents reading the code first-hand; each topic is written to its own `AUDIT-*.md` file. File:line references are against `main` at audit time.

## The codebase at a glance

- Backend: ~145k LOC of Rust source across 23 crates, ~1,896 tracked test functions, 170 sea-orm migrations.
- Entry point (`backend/src/main.rs`): `init_state` (gradient-core) then `start_cache` (gradient-cache) then `serve_web` (gradient-web). The scheduler runs inside the web service as an `Arc<Scheduler>` Extension.
- Largest crates: `gradient-web` 41k, `gradient-worker` 15k, `gradient-migration` 13k, `gradient-scheduler` 11k, `gradient-proto` 9.5k, `gradient-db` 8.7k.
- Frontend: `frontend/` Angular 21, ~16.7k LOC of TypeScript (audited in AUDIT-FRONTEND.md, #483).

## North star: one legible flow

The guiding goal for the refactor, especially for scheduling and self-heal, is a codebase where each behavior reads as a single followable flow rather than logic smeared across many files and two parallel execution models. Today you cannot open one place and see "how a build goes from ready to dispatched" or "how the graph heals itself"; the logic is split across the scheduler, the DB layer, and GC, and duplicated between a reactive path and a proactive path. Every refactor recommendation in these files serves that goal, and the scheduling core has a dedicated cross-cutting plan for it (see AUDIT-SCHEDULER.md).

## The single root cause behind most incidents

The scheduling/self-heal/GC subsystems share one structural defect: **the build-graph state is mutated by two different execution models that must be kept in sync by hand.**

- A reactive single-row path (`update_derivation_build_status`) that validates a transition and fans out all its consequences.
- A family of proactive bulk raw-SQL sweeps (`promote_ready`, `cascade_dependency_failed`, `reconcile_*`, GC deletions) that bypass both the state machine and the reactive side effects, so every caller must remember to re-emit them.

Because healing lives in both models and canonically in neither, the "reconcile then promote" pipeline is copy-pasted at three call sites with divergent ordering, GC adds a seventh independent reimplementation of build-graph reachability, and derived flags (`closure_complete`, `drv_closure_cached`, `edges_complete`, `is_cached`) drift stale-true after GC. Nearly every production incident in the project notes ("stuck Building", "dead zone", "stale-true flag", "InputsUnavailable poison") is an instance of this.

The unifying fix (detailed in AUDIT-SCHEDULER.md) is five coordinated changes: one graph reconciler, one transition entry point with an attached effects hook, one readiness/reachability definition, one decomposed dispatch pipeline, and one information-gathering layer.

## Audit files

| File | Topic | Status | Issue |
|---|---|---|---|
| AUDIT-SCHEDULER.md | Dispatch algorithm, build/eval orchestration, scoring, information gathering, DB state-machine and reconciliation, garbage collection | Done | #476 |
| AUDIT-PROTOCOL.md | Worker protocol and executor, NAR upload path (origins, transports, ingest) | Done | #477 |
| AUDIT-DB.md | DB entity/schema design, migration sprawl | Done | #478 |
| AUDIT-WEB.md | Web/server layer, access control, cross-cutting concerns | Done | #479 |
| AUDIT-TEST.md | Test suite: over-specificity, duplication, compaction/generalization | Done | #480 |
| AUDIT-EVAL-WORKER.md | Eval worker subprocess, resolver, eval pool | Done | #481 |
| AUDIT-FILESTORAGE.md | Storage abstraction (NAR/log/blob, S3 vs local, partial/resumable) | Done | #482 |
| AUDIT-FRONTEND.md | Angular frontend | Done | #483 |

## Cross-cutting observations

- The messiness is concentrated, not uniform. The scheduler (`build.rs` 1992, `jobs.rs` 1900, `dispatch.rs` 1260, `eval.rs` 1249), the protocol/executor (`handler/dispatch.rs` 1467, `nar_import.rs` 1830, `executor/build.rs` 1483), and one web file (`forge_hooks/trigger.rs` 2173) are the god-files. By contrast the web layer overall, the error-handling strategy (typed at the boundary, anyhow internally), and config resolution are well-engineered and worth emulating.
- Derived state stored as flags is the recurring correctness liability (AUDIT-DB.md smell 3, AUDIT-SCHEDULER.md sections 5 and 6). The strategic fix is to make derived facts computed (views/gates-on-read) rather than independently persisted columns that GC and reconcilers must chase.
- Status enums stored as bare ints drive 43 hand-written magic-int SQL literals (AUDIT-DB.md smell 1), the schema-level root of the fragile raw SQL in the scheduler.
- One correctness gap worth prioritizing regardless of the larger refactor: the S3 presigned NAR upload path skips server-side verification entirely (AUDIT-PROTOCOL.md), which can manufacture zombie cached_path rows.
- The test suite is comprehensive but coupled to implementation (983 query-order mock chains) and bloated with ~192 dead manual-runtime wrappers justified by a stale comment; ~1,500-2,000 lines are recoverable at zero coverage loss (AUDIT-TEST.md).

## How this was produced

Parallel agents each read a subsystem first-hand and reported flow diagrams, ranked code smells with file:line evidence, and refactoring recommendations. The main author cross-checked a sample of findings directly. Recommendations are ranked by impact within each file; the scheduling cross-cutting plan (AUDIT-SCHEDULER.md) is the recommended starting point.
