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
mod m20261107_000000_exec_keep_vm_running;
mod m20261108_000000_exec_manual_start;
mod m20261109_000000_cache_metric_external_traffic;
mod m20261110_000000_user_last_seen_version;
mod m20261111_000000_worker_sample_architectures;
mod m20261112_000952_cached_path_file_hash_index;
mod m20261113_000000_drop_project_exec_enabled;
mod m20261114_000000_user_identity;
mod m20261115_000000_drop_cached_path_file_hash_index;
mod m20261116_000000_derivation_output_verified_by_upstream;
mod m20261117_000000_only_evaluate;
mod m20261119_000000_counter_trigger_early_exit;
mod m20261120_000000_task_flake_input_source;
mod m20261121_000000_gradient_run;
mod m20261122_000000_task_repository_fetch;
mod m20261123_000000_incoming_webhook_delivery;
mod m20261124_000000_case_insensitive_username_unique_index;
mod m20261125_000000_add_program_started_at_to_exec_job_attempt;
mod m20261126_000000_delete_pending_requests_for_granted_access;
mod m20261127_000000_delete_worker_registration_for_deleted_project_and_add_project_foreign_key;
mod m20261128_000000_add_dispatched_at_desc_index_to_dispatched_job;
mod m20261128_000001_add_view_permission_bit_to_all_roles;
mod m20261129_000000_integration_token_expiry_columns;
mod m20261129_000001_add_restrict_evaluation_fetches_and_extra_allowed_uri_prefixes_to_project;
mod m20261130_000000_add_view_and_read_store_bits_to_existing_api_keys;
mod m20261201_000000_restrict_user_foreign_keys;
mod m20261201_000001_case_insensitive_email_unique_index;
mod m20261201_000002_add_job_id_dispatched_at_desc_index_to_dispatched_job_concurrently;
mod m20261201_000003_evaluation_previous_next_set_null_on_delete;
mod m20261201_000004_add_retention_delete_indexes;
mod m20261202_000000_add_view_project_permission_bit_to_existing_api_keys_not_pinned_to_cache;
mod m20261203_000000_project_owner_team_and_direct_role_precedence;
mod m20261204_000000_create_retired_cache_key_table_and_add_previous_signature_column;
mod m20261206_000000_add_sign_outputs_to_project_cache;
mod m20261207_000000_add_abort_record_to_evaluation_and_one_commit_row_per_project_hash;
mod m20261208_000000_move_project_integrations_and_github_app_installations_to_git_host_connections_owned_by_instance_team_or_project_and_add_task_git_host_connection_and_task_report_status_and_replace_status_report_actions;
mod m20261209_000000_add_pin_columns_to_cached_path_signature;
mod m20261210_000000_add_use_superuser_rights_bit_to_existing_api_keys_not_pinned_to_cache_owned_by_superuser_with_all_project_permissions;
mod m20261211_000000_add_cache_permission_column_to_api_and_move_pinned_api_key_permissions_to_cache_permission;

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
            Box::new(m20261107_000000_exec_keep_vm_running::Migration),
            Box::new(m20261108_000000_exec_manual_start::Migration),
            Box::new(m20261109_000000_cache_metric_external_traffic::Migration),
            Box::new(m20261110_000000_user_last_seen_version::Migration),
            Box::new(m20261111_000000_worker_sample_architectures::Migration),
            Box::new(m20261112_000952_cached_path_file_hash_index::Migration),
            Box::new(m20261113_000000_drop_project_exec_enabled::Migration),
            Box::new(m20261114_000000_user_identity::Migration),
            Box::new(m20261115_000000_drop_cached_path_file_hash_index::Migration),
            Box::new(m20261116_000000_derivation_output_verified_by_upstream::Migration),
            Box::new(m20261117_000000_only_evaluate::Migration),
            Box::new(m20261119_000000_counter_trigger_early_exit::Migration),
            Box::new(m20261120_000000_task_flake_input_source::Migration),
            Box::new(m20261121_000000_gradient_run::Migration),
            Box::new(m20261122_000000_task_repository_fetch::Migration),
            Box::new(m20261123_000000_incoming_webhook_delivery::Migration),
            Box::new(m20261124_000000_case_insensitive_username_unique_index::Migration),
            Box::new(m20261125_000000_add_program_started_at_to_exec_job_attempt::Migration),
            Box::new(m20261126_000000_delete_pending_requests_for_granted_access::Migration),
            Box::new(m20261127_000000_delete_worker_registration_for_deleted_project_and_add_project_foreign_key::Migration),
            Box::new(m20261128_000000_add_dispatched_at_desc_index_to_dispatched_job::Migration),
            Box::new(m20261128_000001_add_view_permission_bit_to_all_roles::Migration),
            Box::new(m20261129_000000_integration_token_expiry_columns::Migration),
            Box::new(m20261129_000001_add_restrict_evaluation_fetches_and_extra_allowed_uri_prefixes_to_project::Migration),
            Box::new(m20261130_000000_add_view_and_read_store_bits_to_existing_api_keys::Migration),
            Box::new(m20261201_000000_restrict_user_foreign_keys::Migration),
            Box::new(m20261201_000001_case_insensitive_email_unique_index::Migration),
            Box::new(m20261201_000002_add_job_id_dispatched_at_desc_index_to_dispatched_job_concurrently::Migration),
            Box::new(m20261201_000003_evaluation_previous_next_set_null_on_delete::Migration),
            Box::new(m20261201_000004_add_retention_delete_indexes::Migration),
            Box::new(
                m20261202_000000_add_view_project_permission_bit_to_existing_api_keys_not_pinned_to_cache::Migration,
            ),
            Box::new(m20261203_000000_project_owner_team_and_direct_role_precedence::Migration),
            Box::new(m20261204_000000_create_retired_cache_key_table_and_add_previous_signature_column::Migration),
            Box::new(m20261206_000000_add_sign_outputs_to_project_cache::Migration),
            Box::new(m20261207_000000_add_abort_record_to_evaluation_and_one_commit_row_per_project_hash::Migration),
            Box::new(m20261208_000000_move_project_integrations_and_github_app_installations_to_git_host_connections_owned_by_instance_team_or_project_and_add_task_git_host_connection_and_task_report_status_and_replace_status_report_actions::Migration),
            Box::new(m20261209_000000_add_pin_columns_to_cached_path_signature::Migration),
            Box::new(m20261210_000000_add_use_superuser_rights_bit_to_existing_api_keys_not_pinned_to_cache_owned_by_superuser_with_all_project_permissions::Migration),
            Box::new(m20261211_000000_add_cache_permission_column_to_api_and_move_pinned_api_key_permissions_to_cache_permission::Migration),
        ]
    }
}
