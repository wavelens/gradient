/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod admin_tasks;
pub mod base_workers;
pub mod build_attempt;
pub mod cache_reach;
pub mod cache_storage;
pub mod cache_upstream;
pub mod chunked;
pub mod closure;
pub mod connection;
pub mod consistency;
pub mod context;
pub mod dep_closure;
pub mod dependency_graph;
pub mod derivation;
pub mod draining;
pub mod drv_output_spec;
pub mod gc;
pub mod graph_sql;
pub mod org_cache;
pub mod org_derivations;
pub mod org_workers;
pub mod promotion;
pub mod permissions;
pub mod pool;
pub mod project_board;
pub mod reachability;
pub mod reconcile;
pub mod recovery;
pub mod retention;
pub mod rollup;
pub mod runtime_closure;
pub mod state_machine;
pub mod status;
pub mod status_reactor;
pub mod status_sql;

pub use self::build_attempt::*;
pub use self::cache_reach::*;
pub use self::cache_storage::{
    MissingInputDiagnosis, STORAGE_HEADROOM_BYTES, cache_used_bytes,
    clear_closure_complete_for_referrers, clear_gate_flags_for_hashes, demote_cached_output,
    demote_output_only_cached_deps, demote_referrers_of, demote_unbacked_trusted_outputs,
    diagnose_missing_input, instance_used_bytes, org_caches_all_full, org_writable_caches,
};
pub use self::cache_upstream::{
    GradientProtoUpstream, UpstreamAccum, UpstreamEndpoint, gradient_proto_upstreams_for_org,
    upstream_endpoints_for_org, upstream_urls_for_org, upstream_urls_for_orgs,
    upsert_upstream_metrics,
};
pub use self::chunked::{IN_CHUNK_SIZE, fetch_in_chunks, for_each_chunk};
pub use self::closure::*;
pub use self::connection::*;
pub use self::consistency::{ConsistencyReport, graph_consistency_report};
pub use self::context::DbContext;
pub use self::dep_closure::*;
pub use self::dependency_graph::*;
pub use self::derivation::*;
pub use self::drv_output_spec::DrvOutputSpec;
pub use self::draining::{park_active_evals, unpark_draining_evals};
pub use self::gc::*;
pub use self::graph_sql::{
    ClosureDirection, dependency_closure_cte, eval_closure_cte, reachable_derivations_cte,
};
pub use self::org_cache::org_has_writable_cache;
pub use self::org_derivations::derivation_ids_for_org;
pub use self::promotion::{
    cascade_dependency_failed, find_ready_anchors, mark_edges_complete_for_eval,
    promote_dependents, promote_ready, propagate_closure_complete,
    reconcile_cached_anchors_for_eval, reconcile_closure_complete, reconcile_dependency_failed,
    reconcile_drv_closure_cached, requeue_failed_anchors, requeue_failed_closure_for_eval,
};
pub use self::reachability::{
    build_jobs_for_derivation, build_jobs_for_derivations, derivation_is_reachable,
    eval_anchor_statuses, evals_referencing_derivation,
};
pub use self::org_workers::org_has_eval_capable_worker_registration;
pub use self::pool::{CacheDb, WebDb, WorkerDb};
pub use self::project_board::*;
pub use self::reconcile::{ReconcileReport, ReconcileScope, reconcile_build_graph};
pub use self::recovery::recover_interrupted_work;
pub use self::runtime_closure::*;
pub use self::status::*;
pub use self::status_reactor::{NoReactor, StatusReactor};
