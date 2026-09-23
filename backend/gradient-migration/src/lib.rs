/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::disallowed_methods,
    reason = "migrations are not gated: they are not independently executable"
)]

pub use sea_orm_migration::prelude::*;

mod m20241101_000000_baseline;
mod m20260619_010000_globalize_derivation;
mod m20260619_020000_derivation_build_anchor;
mod m20260619_030000_build_job_and_attempt;
mod m20260620_000001_create_github_installation;
mod m20260620_000002_backfill_drop_org_installation;
mod m20260620_000003_index_build_job_derivation;
mod m20260620_000004_fix_github_installation_created_at_tz;
mod m20260620_000005_derivation_output_upstream;
mod m20260621_000001_derivation_build_edges_complete;
mod m20260623_000000_create_derivation_input_source;
mod m20260624_000001_closure_complete;
mod m20260624_000002_dispatch_indexes;
mod m20260624_000003_cached_path_reference;
mod m20260625_000001_derivation_output_file_hash;
mod m20260626_000000_create_table_upstream_metric;
mod m20260626_000001_build_log_chunk_cascade;
mod m20260626_000002_drop_acknowledged_derivation;
mod m20260627_000000_derivation_build_edges_unresolved;
mod m20260629_000000_derivation_build_drv_closure_cached;
mod m20260703_000000_drop_derivation_build_prefer_local_build;
mod m20260703_000001_terminal_failed_partial_index;
mod m20260704_000000_flag_partial_indexes;
mod m20260705_000000_upstream_metric_by_url;
mod m20260706_000000_build_attempt_build_job_set_null;
mod m20260706_000001_disable_jit;
mod m20260709_000000_input_update_discover_only;
mod m20260821_000000_debug_info;
mod m20260822_000000_rename_project_to_task;
mod m20260822_000001_rename_organization_to_project;
mod m20260827_000000_metric_rollup_scope_project;
mod m20260827_000001_commit_hash_index;
mod m20260904_000000_dispatched_job_phase;
mod m20260904_000001_graph_edge_indexes;
mod m20260905_000000_invites;
mod m20260907_000000_dispatched_job_job_id;
mod m20260907_000001_base_worker_auto_enable;
mod m20260908_000000_derivation_walked;
mod m20260908_000001_cached_path_missing_references;
mod m20260908_000002_derivation_build_readiness;
mod m20260909_000000_build_attempt_indexes;
mod m20260911_000000_edge_bloat_and_dead_indexes;
mod m20260911_000001_drop_derivation_closure;
mod m20260915_000000_demand;
mod m20260915_000001_cached_path_confirmed;
mod m20260916_000000_path_retention;
mod m20260917_000001_derivation_build_demanded;
mod m20260917_000002_outbox;
mod m20260917_000003_open_work_indexes;
mod m20260918_000000_gc_freshness_indexes;
mod m20260918_000001_dispatch_window_columns;
mod m20260918_000002_derivation_unwalked_inputs;
mod m20260919_000000_one_graph;
mod m20260919_000001_drop_cached_path_reference;
mod m20260919_000002_derivation_build_probed;
mod m20260922_000000_derivation_build_open;
mod m20260923_000000_derivation_build_fetchable_unwhole;
mod m20260923_000001_derivation_build_open_aborted;
pub mod m20260923_000002_evaluation_anchor_counters;
mod m20260923_000003_entry_point_unique_eval;
mod m20260923_000004_dispatched_job_eval_index;
mod m20260923_000005_dispatched_job_open_unique;
mod m20260923_000006_user_stars;
mod m20260923_000007_evaluation_walk_mode;
mod m20260923_000008_evaluation_task_commit_index;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20241101_000000_baseline::Migration),
            Box::new(m20260619_010000_globalize_derivation::Migration),
            Box::new(m20260619_020000_derivation_build_anchor::Migration),
            Box::new(m20260619_030000_build_job_and_attempt::Migration),
            Box::new(m20260620_000001_create_github_installation::Migration),
            Box::new(m20260620_000002_backfill_drop_org_installation::Migration),
            Box::new(m20260620_000003_index_build_job_derivation::Migration),
            Box::new(m20260620_000004_fix_github_installation_created_at_tz::Migration),
            Box::new(m20260620_000005_derivation_output_upstream::Migration),
            Box::new(m20260621_000001_derivation_build_edges_complete::Migration),
            Box::new(m20260623_000000_create_derivation_input_source::Migration),
            Box::new(m20260624_000001_closure_complete::Migration),
            Box::new(m20260624_000002_dispatch_indexes::Migration),
            Box::new(m20260624_000003_cached_path_reference::Migration),
            Box::new(m20260625_000001_derivation_output_file_hash::Migration),
            Box::new(m20260626_000000_create_table_upstream_metric::Migration),
            Box::new(m20260626_000001_build_log_chunk_cascade::Migration),
            Box::new(m20260626_000002_drop_acknowledged_derivation::Migration),
            Box::new(m20260627_000000_derivation_build_edges_unresolved::Migration),
            Box::new(m20260629_000000_derivation_build_drv_closure_cached::Migration),
            Box::new(m20260703_000000_drop_derivation_build_prefer_local_build::Migration),
            Box::new(m20260703_000001_terminal_failed_partial_index::Migration),
            Box::new(m20260704_000000_flag_partial_indexes::Migration),
            Box::new(m20260705_000000_upstream_metric_by_url::Migration),
            Box::new(m20260706_000000_build_attempt_build_job_set_null::Migration),
            Box::new(m20260706_000001_disable_jit::Migration),
            Box::new(m20260709_000000_input_update_discover_only::Migration),
            Box::new(m20260821_000000_debug_info::Migration),
            Box::new(m20260822_000000_rename_project_to_task::Migration),
            Box::new(m20260822_000001_rename_organization_to_project::Migration),
            Box::new(m20260827_000000_metric_rollup_scope_project::Migration),
            Box::new(m20260827_000001_commit_hash_index::Migration),
            Box::new(m20260904_000000_dispatched_job_phase::Migration),
            Box::new(m20260904_000001_graph_edge_indexes::Migration),
            Box::new(m20260905_000000_invites::Migration),
            Box::new(m20260907_000000_dispatched_job_job_id::Migration),
            Box::new(m20260907_000001_base_worker_auto_enable::Migration),
            Box::new(m20260908_000000_derivation_walked::Migration),
            Box::new(m20260908_000001_cached_path_missing_references::Migration),
            Box::new(m20260908_000002_derivation_build_readiness::Migration),
            Box::new(m20260909_000000_build_attempt_indexes::Migration),
            Box::new(m20260911_000000_edge_bloat_and_dead_indexes::Migration),
            Box::new(m20260911_000001_drop_derivation_closure::Migration),
            Box::new(m20260915_000000_demand::Migration),
            Box::new(m20260915_000001_cached_path_confirmed::Migration),
            Box::new(m20260916_000000_path_retention::Migration),
            Box::new(m20260917_000001_derivation_build_demanded::Migration),
            Box::new(m20260917_000002_outbox::Migration),
            Box::new(m20260917_000003_open_work_indexes::Migration),
            Box::new(m20260918_000000_gc_freshness_indexes::Migration),
            Box::new(m20260918_000001_dispatch_window_columns::Migration),
            Box::new(m20260918_000002_derivation_unwalked_inputs::Migration),
            Box::new(m20260919_000000_one_graph::Migration),
            Box::new(m20260919_000001_drop_cached_path_reference::Migration),
            Box::new(m20260919_000002_derivation_build_probed::Migration),
            Box::new(m20260922_000000_derivation_build_open::Migration),
            Box::new(m20260923_000000_derivation_build_fetchable_unwhole::Migration),
            Box::new(m20260923_000001_derivation_build_open_aborted::Migration),
            Box::new(m20260923_000002_evaluation_anchor_counters::Migration),
            Box::new(m20260923_000003_entry_point_unique_eval::Migration),
            Box::new(m20260923_000004_dispatched_job_eval_index::Migration),
            Box::new(m20260923_000005_dispatched_job_open_unique::Migration),
            Box::new(m20260923_000006_user_stars::Migration),
            Box::new(m20260923_000007_evaluation_walk_mode::Migration),
            Box::new(m20260923_000008_evaluation_task_commit_index::Migration),
        ]
    }
}
