/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod admin_tasks;
pub mod assignment_record;
pub mod base_workers;
pub mod build_attempt;
pub mod build_watchdog;
pub mod cache_metric;
pub mod cache_reach;
pub mod cache_storage;
pub mod cache_upstream;
pub mod cache_usage;
pub mod can_start;
pub mod chunked;
pub mod closure;
pub mod cluster;
pub mod connection;
pub mod consistency;
pub mod context;
pub mod dashboard;
pub mod debug_info;
pub mod dep_counts;
pub mod dependency_graph;
pub mod draining;
pub mod eval_counters;
pub mod eval_watchdog;
pub mod events;
pub mod gc;
pub mod graph_sql;
pub mod infra_metric;
pub mod pending_deliveries;
pub mod permissions;
pub mod pool;
pub mod priority;
pub mod project_cache;
pub mod project_derivations;
pub mod project_workers;
pub mod promotion;
pub mod reachability;
pub mod recovery;
pub mod repair;
pub mod retention;
pub mod rollup;
pub mod runtime_can_start;
pub mod runtime_closure;
pub mod runtime_dependencies;
pub mod shared_build_guard;
pub mod sql;
pub mod startable_set;
pub mod state_machine;
pub mod status;
pub mod status_sql;
pub mod storage_migrations;
pub mod task_board;
pub mod walk_completeness;

#[cfg(test)]
pub(crate) mod test_ctx;

pub use self::assignment_record::{
    BUILD_KEY_PREFIX, ClaimGate, EVAL_KEY_PREFIX, abandon_all_open_assignments,
    abandon_open_assignment, abandon_open_assignments, abandon_open_assignments_for_jobs,
    abandon_open_assignments_for_worker, build_job_key_sql, claim_assignment, eval_attempts,
    eval_job_key_sql, latest_eval_jobs, no_open_assignment_predicate,
};
pub use self::build_attempt::*;
pub use self::build_watchdog::stranded_building_shared_builds;
pub use self::cache_reach::*;
pub use self::cache_storage::{
    MissingInputDiagnosis, STORAGE_HEADROOM_BYTES, cache_used_bytes, demote_cached_output,
    demote_output_only_cached_deps, demote_parents_of, diagnose_missing_input, instance_used_bytes,
    project_caches_all_full, project_writable_caches, unconfirmed_cached_path_count,
};
pub use self::cache_upstream::{
    GradientProtoUpstream, UpstreamAccum, UpstreamEndpoint,
    gradient_proto_upstream_caches_for_project, upsert_upstream_metrics,
    upstream_endpoints_for_project, upstream_urls_for_projects,
};
pub use self::can_start::{
    NeedMoved, Repaired, SeedLock, SettledNeed, SharedBuildLock, advance_fetchable,
    became_fetchable, can_start_scope, lock_seed_shared_builds, lock_shared_builds,
    lost_fetchability, promote, promote_closure, recount_wanted, repair_can_start,
    repair_fetchable, seed_blocking_deps, settle_skipped, unpromote_drv_owners, unpromote_ungated,
    unwalk_derivations, update_and_settle_need,
};
pub use self::chunked::{IN_CHUNK_SIZE, fetch_in_chunks, for_each_chunk};
pub use self::closure::*;
pub use self::cluster::*;
pub use self::connection::*;
pub use self::consistency::{ConsistencyReport, graph_consistency_report};
pub use self::context::{DbContext, ProbeRequests};
pub use self::debug_info::{
    DebugInfoTarget, carries_debug_info, index_cached_path, lookup_for_cache, pending_debug_index,
};
pub use self::dep_counts::*;
pub use self::dependency_graph::*;
pub use self::draining::{park_active_evals, unpark_draining_evals};
pub use self::eval_counters::{
    EvalCounters, eval_counters, fold_shared_build_deltas, in_flight_counters,
    recount_eval_shared_build_counters, recount_evaluations,
};
pub use self::eval_watchdog::{LostCompletion, lost_eval_completions};
pub use self::gc::*;
pub use self::graph_sql::{
    ClosureDirection, begin_walk, dependency_closure_cte, eval_closure_cte,
    reachable_derivations_cte,
};
pub use self::pool::{CacheDb, WebDb, WorkerDb};
pub use self::priority::{prioritize_build_closure, prioritize_evaluation};
pub use self::project_cache::project_has_writable_cache;
pub use self::project_derivations::derivation_ids_for_project;
pub use self::project_workers::project_has_eval_capable_worker_registration;
pub use self::promotion::{
    cascade_dependency_failed, find_startable_shared_builds, find_startable_shared_builds_among,
    repair_cached_shared_builds_for_eval, repair_dependency_failed, requeue_failed_closure,
    requeue_failed_shared_builds, substitute_created_shared_builds,
};
pub use self::reachability::{
    Adopted, adopt_pending_closure, adopt_pending_closures, build_jobs_for_derivation,
    build_jobs_for_derivations, derivation_is_reachable, derivations_with_hashes,
    eval_any_shared_build_failed, eval_blocked, evals_referencing_derivation,
    evals_referencing_derivations, inherit_names, pending_orphan_frontier, pending_orphans_among,
    private_output_hashes, producers_of_hashes, shared_build_status,
};
pub use self::recovery::recover_interrupted_work;
pub use self::repair::{RepairReport, RepairScope, repair_build_graph};
pub use self::runtime_can_start::{
    Seeded, complete_among, complete_output_hashes, lock_cached_paths,
    recount_missing_runtime_deps, retire_outputs, ripple_shared_builds_complete,
    ripple_shared_builds_incomplete, seed_runtime_deps,
};
pub use self::runtime_closure::*;
pub use self::runtime_dependencies::{
    adopt_referenced_outputs, insert_runtime_dependencies, producers_of_tokens,
};
pub use self::startable_set::{StartableMoves, StartableSet};
pub use self::status::*;
pub use self::task_board::*;
pub use self::walk_completeness::{recount_walk_completeness, seed_walk_completeness, unwalk};
