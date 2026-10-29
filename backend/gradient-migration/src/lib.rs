/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::disallowed_methods,
    reason = "migrations are not independently executable"
)]

pub use sea_orm_migration::prelude::*;

mod m20261013_000000_baseline;
mod m20261014_000000_evaluation_branch;
mod m20261014_000001_auto_concurrency;
mod m20261015_000000_evaluation_metric_per_evaluation;
mod m20261015_000001_output_cache_by_hash;
mod m20261015_000002_pull_request_summary;
mod m20261016_000000_metric_input_pnames;
mod m20261016_000001_metric_pname_without_version;
mod m20261016_000002_team_webhooks;
mod m20261018_000000_worker_network_capability;
mod m20261019_000000_eval_cache_per_cache;
mod m20261024_000000_output_path_known_after_build;
mod m20261024_000001_default_trigger;
mod m20261024_000002_entry_point_licenses;
mod m20261025_000000_derivation_build_probed_at;
mod m20261026_000000_eval_cache_purged_at;
mod m20261027_000000_eval_cost_estimates;
mod m20261028_000000_split_evaluation;
mod m20261028_000001_task_multi_node_evaluation;
mod m20261029_000000_eval_cost_report;
mod m20261029_000000_exec_jobs;
mod m20261030_000000_eval_set_cost_max_peak_rss;
mod m20261031_000000_counter_trigger_per_statement;
mod m20261101_000000_pending_delivery_due_order;
mod m20261102_000000_project_secrets;
mod m20261103_000000_exec_job_outputs_tokens;
mod m20261104_000000_exec_clusters;
mod m20261105_000000_partial_substitution;
mod m20261106_000000_task_exec_enabled;

pub struct UpgradeFloor {
    pub release: &'static str,
    pub last_migration: &'static str,
    pub storage_migrations: &'static [&'static str],
}

pub const UPGRADE_FLOOR: UpgradeFloor = UpgradeFloor {
    release: "v2.0.0",
    last_migration: "m20261012_000001_wildcard_grammar",
    storage_migrations: &["m20261001_000000_shard_build_logs"],
};

/// The SQL of the newest migration defining a function. Tests compare it with the Rust side.
pub fn function_sql(name: &str) -> &'static str {
    let replaced = format!("CREATE OR REPLACE FUNCTION public.{name}(");
    let created = format!("CREATE FUNCTION public.{name}(");
    [
        m20261105_000000_partial_substitution::UP,
        include_str!("m20261013_000000_baseline.sql"),
    ]
    .into_iter()
    .find_map(|sql| {
        let start = sql.find(&replaced).or_else(|| sql.find(&created))?;
        let end = start + sql[start..].find("$$;")? + 3;
        Some(&sql[start..end])
    })
    .expect("a migration defining the function")
}

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20261013_000000_baseline::Migration),
            Box::new(m20261014_000000_evaluation_branch::Migration),
            Box::new(m20261014_000001_auto_concurrency::Migration),
            Box::new(m20261015_000000_evaluation_metric_per_evaluation::Migration),
            Box::new(m20261015_000001_output_cache_by_hash::Migration),
            Box::new(m20261015_000002_pull_request_summary::Migration),
            Box::new(m20261016_000000_metric_input_pnames::Migration),
            Box::new(m20261016_000001_metric_pname_without_version::Migration),
            Box::new(m20261016_000002_team_webhooks::Migration),
            Box::new(m20261018_000000_worker_network_capability::Migration),
            Box::new(m20261019_000000_eval_cache_per_cache::Migration),
            Box::new(m20261024_000000_output_path_known_after_build::Migration),
            Box::new(m20261024_000001_default_trigger::Migration),
            Box::new(m20261024_000002_entry_point_licenses::Migration),
            Box::new(m20261025_000000_derivation_build_probed_at::Migration),
            Box::new(m20261026_000000_eval_cache_purged_at::Migration),
            Box::new(m20261027_000000_eval_cost_estimates::Migration),
            Box::new(m20261028_000000_split_evaluation::Migration),
            Box::new(m20261028_000001_task_multi_node_evaluation::Migration),
            Box::new(m20261029_000000_exec_jobs::Migration),
            Box::new(m20261029_000000_eval_cost_report::Migration),
            Box::new(m20261030_000000_eval_set_cost_max_peak_rss::Migration),
            Box::new(m20261031_000000_counter_trigger_per_statement::Migration),
            Box::new(m20261101_000000_pending_delivery_due_order::Migration),
            Box::new(m20261102_000000_project_secrets::Migration),
            Box::new(m20261103_000000_exec_job_outputs_tokens::Migration),
            Box::new(m20261104_000000_exec_clusters::Migration),
            Box::new(m20261105_000000_partial_substitution::Migration),
            Box::new(m20261106_000000_task_exec_enabled::Migration),
        ]
    }
}
