/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

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
        ]
    }
}
