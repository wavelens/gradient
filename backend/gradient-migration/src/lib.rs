/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub use sea_orm_migration::prelude::*;

mod m20241107_135027_create_table_user;
mod m20241107_135442_create_table_organization;
mod m20241107_135941_create_table_project;
mod m20241107_140540_create_table_feature;
mod m20241107_140545_create_table_commit;
mod m20241107_140550_create_table_server;
mod m20241107_140560_create_table_build;
mod m20241107_140600_create_table_evaluation;
mod m20241107_155000_create_table_build_dependency;
mod m20241107_155020_create_table_api;
mod m20241107_156000_create_table_build_feature;
mod m20241107_156100_create_table_server_feature;
mod m20241107_156110_create_table_server_architecture;
mod m20241107_156120_create_table_cache;
mod m20241107_156130_create_table_organization_cache;
mod m20241107_156140_create_table_role;
mod m20241107_156150_create_table_organization_user;
mod m20241107_156160_create_table_build_output;
mod m20241107_156170_create_table_build_output_signature;
mod m20250705_000000_create_table_direct_build;
mod m20250705_000001_make_evaluation_project_nullable;
mod m20250707_000000_add_email_verification_to_user;
mod m20250917_000000_add_managed_field_to_entities;
mod m20260323_000000_create_table_entry_point;
mod m20260323_000001_add_updated_at_to_evaluation;
mod m20260326_000000_add_public_to_organization;
mod m20260326_000001_add_public_to_cache;
mod m20260328_000000_add_max_concurrent_builds_to_server;
mod m20260328_000001_split_cache_signing_key;
mod m20260329_000000_create_table_webhook;
mod m20260401_000000_create_table_cache_metric;
mod m20260401_000001_add_mode_to_organization_cache;
mod m20260401_000002_create_table_cache_upstream;
mod m20260402_000000_add_last_fetched_at_to_build_output;
mod m20260405_000000_add_log_id_to_build;
mod m20260405_000002_add_build_time_ms_to_build;
mod m20260405_000003_add_keep_evaluations_to_project;
mod m20260406_000001_add_eval_to_entry_point;
mod m20260407_000000_renumber_evaluation_status;
mod m20260407_000001_add_nar_size_to_build_output;
mod m20260408_000000_split_build_into_derivation;
mod m20260408_000001_evaluation_messages;
mod m20260409_000000_add_ci_reporter_to_project;
mod m20260410_000000_add_fetching_evaluation_status;
mod m20260411_000000_rename_server_to_build_machine;
mod m20260412_000000_add_superuser_to_user;
mod m20260412_000001_create_worker_registration;
mod m20260412_000002_convert_architecture_to_string;
mod m20260412_000003_drop_ssh_server_tables;
mod m20260412_000004_add_managed_to_worker_registration;
mod m20260413_000000_add_url_to_worker_registration;
mod m20260414_000000_add_active_to_worker_registration;
mod m20260416_000000_create_table_cached_path;
mod m20260418_000000_rename_feature_add_kind;
mod m20260418_000001_add_name_to_worker_registration;
mod m20260420_000000_add_flake_source_to_evaluation;
mod m20260421_000000_create_table_integration;
mod m20260421_000002_drop_use_nix_store_from_organization;
mod m20260421_000003_create_table_build_product;
mod m20260422_000000_rename_worker_name_and_add_created_by;
mod m20260422_000001_add_display_name_to_integration;
mod m20260422_000002_add_enable_caps_to_worker_registration;
mod m20260422_000003_add_deriver_to_cached_path;
mod m20260425_000000_replace_build_server_with_worker;
mod m20260430_000000_normalize_hash_columns;
mod m20260501_000001_add_repo_check_id;
mod m20260502_000000_hash_api_keys;
mod m20260502_000001_drop_file_columns_from_derivation_output;
mod m20260502_000002_add_oidc_identity_to_user;
mod m20260504_000000_cached_path_signature_to_bytea;
mod m20260504_000001_add_via_to_build;
mod m20260504_000002_add_external_cached_to_build;
mod m20260505_000000_index_build_dispatch;
mod m20260505_000001_api_key_lifecycle;
mod m20260505_000002_create_table_session;
mod m20260505_000003_create_table_audit_log;
mod m20260505_000004_create_table_webhook_delivery;
mod m20260506_000000_add_waiting_reason_to_evaluation;
mod m20260506_000001_index_derivation_output_cache_lookup;
mod m20260507_000000_create_table_project_trigger;
mod m20260507_000001_add_trigger_to_evaluation;
mod m20260507_000002_drop_inbound_integration;
mod m20260507_000003_unique_active_evaluation_per_project;
mod m20260507_000004_move_concurrency_to_project;
mod m20260507_000005_evaluation_concurrent_flag;
mod m20260507_000006_add_subtype_to_build_product;
mod m20260507_000007_add_sign_cache_to_project;
mod m20260508_000000_rename_project_evaluation_wildcard;
mod m20260510_000000_seed_github_app_integrations;
mod m20260510_000001_api_key_options;
mod m20260510_000002_add_managed_to_role;
mod m20260511_000000_strip_derivation_path_prefix;
mod m20260513_000000_add_local_priority_to_cache;
mod m20260519_000000_normalize_derivation_columns;
mod m20260519_000001_create_build_request_blob;
mod m20260519_000002_create_upload_session;
mod m20260519_000003_add_organization_hide_build_requests;
mod m20260519_000004_drop_direct_build;
mod m20260521_000000_tag_waiting_reason_kind;
mod m20260522_000000_cached_path_signature_fetch_stats;
mod m20260522_000001_create_flake_input_override;
mod m20260524_000000_create_table_project_action;
mod m20260524_000001_create_table_project_action_delivery;
mod m20260524_000002_drop_table_webhook_delivery;
mod m20260524_000003_drop_table_webhook;
mod m20260524_000004_drop_table_project_integration;
mod m20260525_000000_create_table_cache_role;
mod m20260525_000001_create_table_cache_user;
mod m20260525_000002_add_cache_to_api;
mod m20260525_000003_create_admin_task;
mod m20260525_000004_evaluation_check_run_ids;
mod m20260527_000000_evaluation_source_comment;
mod m20260527_000001_add_allowed_ips_to_api;
mod m20260527_000002_add_allowed_ips_to_integration;
mod m20260528_000000_create_table_cli_device_authorization;
mod m20260529_000000_add_cache_upstream_kind;
mod m20260603_000000_add_build_failure_retry_fields;
mod m20260604_000001_add_derivation_scoring_columns;
mod m20260604_000002_create_derivation_metric;
mod m20260605_000000_index_hot_lookups;
mod m20260605_000001_create_build_log_chunk;
mod m20260606_000000_add_derivation_fixed_output;
mod m20260607_000000_add_max_storage_to_cache;
mod m20260607_000001_create_table_phase_event;
mod m20260607_000002_create_table_dispatched_job;
mod m20260607_000003_create_table_worker_sample;
mod m20260607_000004_create_table_worker_connection;
mod m20260607_000005_create_table_acknowledged_derivation;
mod m20260607_000006_create_table_metric_rollup;
mod m20260607_000007_add_phase_timing_to_build;
mod m20260607_000008_add_phase_timing_to_evaluation;
mod m20260607_000009_add_queued_at_to_build;
mod m20260607_000010_add_network_to_derivation_metric;
mod m20260608_000001_add_dispatched_job_instance_context;
mod m20260610_000001_create_build_attempt;
mod m20260610_000002_backfill_build_attempt;
mod m20260610_000003_slim_build_and_dispatched_job;
mod m20260612_000001_add_started_by_to_evaluation;
mod m20260613_000001_add_dep_closure_counts;
mod m20260614_000001_eval_cache_store;
mod m20260614_000002_add_scim_to_user;
mod m20260615_000001_create_evaluation_metric;
mod m20260615_000002_create_evaluation_attr_cost;
mod m20260615_000003_create_flake_output_node;
mod m20260615_000004_create_base_worker;
mod m20260615_000005_create_organization_base_worker;
mod m20260616_000001_add_kind_to_evaluation;
mod m20260616_000002_create_evaluation_input_update;
mod m20260616_000003_create_open_pr_state;
mod m20260617_000001_add_substituted_to_build;
mod m20260619_000001_drop_cached_path_store_path;
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

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20241107_135027_create_table_user::Migration),
            Box::new(m20241107_135442_create_table_organization::Migration),
            Box::new(m20241107_135941_create_table_project::Migration),
            Box::new(m20241107_140540_create_table_feature::Migration),
            Box::new(m20241107_140545_create_table_commit::Migration),
            Box::new(m20241107_140600_create_table_evaluation::Migration),
            Box::new(m20241107_140550_create_table_server::Migration),
            Box::new(m20241107_140560_create_table_build::Migration),
            Box::new(m20241107_155000_create_table_build_dependency::Migration),
            Box::new(m20241107_155020_create_table_api::Migration),
            Box::new(m20241107_156000_create_table_build_feature::Migration),
            Box::new(m20241107_156100_create_table_server_feature::Migration),
            Box::new(m20241107_156110_create_table_server_architecture::Migration),
            Box::new(m20241107_156120_create_table_cache::Migration),
            Box::new(m20241107_156130_create_table_organization_cache::Migration),
            Box::new(m20241107_156140_create_table_role::Migration),
            Box::new(m20241107_156150_create_table_organization_user::Migration),
            Box::new(m20241107_156160_create_table_build_output::Migration),
            Box::new(m20241107_156170_create_table_build_output_signature::Migration),
            Box::new(m20250705_000000_create_table_direct_build::Migration),
            Box::new(m20250705_000001_make_evaluation_project_nullable::Migration),
            Box::new(m20250707_000000_add_email_verification_to_user::Migration),
            Box::new(m20250917_000000_add_managed_field_to_entities::Migration),
            Box::new(m20260323_000000_create_table_entry_point::Migration),
            Box::new(m20260323_000001_add_updated_at_to_evaluation::Migration),
            Box::new(m20260326_000000_add_public_to_organization::Migration),
            Box::new(m20260326_000001_add_public_to_cache::Migration),
            Box::new(m20260328_000000_add_max_concurrent_builds_to_server::Migration),
            Box::new(m20260328_000001_split_cache_signing_key::Migration),
            Box::new(m20260329_000000_create_table_webhook::Migration),
            Box::new(m20260401_000000_create_table_cache_metric::Migration),
            Box::new(m20260401_000001_add_mode_to_organization_cache::Migration),
            Box::new(m20260401_000002_create_table_cache_upstream::Migration),
            Box::new(m20260402_000000_add_last_fetched_at_to_build_output::Migration),
            Box::new(m20260405_000000_add_log_id_to_build::Migration),
            Box::new(m20260405_000002_add_build_time_ms_to_build::Migration),
            Box::new(m20260405_000003_add_keep_evaluations_to_project::Migration),
            Box::new(m20260406_000001_add_eval_to_entry_point::Migration),
            Box::new(m20260407_000000_renumber_evaluation_status::Migration),
            Box::new(m20260407_000001_add_nar_size_to_build_output::Migration),
            Box::new(m20260408_000000_split_build_into_derivation::Migration),
            Box::new(m20260408_000001_evaluation_messages::Migration),
            Box::new(m20260409_000000_add_ci_reporter_to_project::Migration),
            Box::new(m20260410_000000_add_fetching_evaluation_status::Migration),
            Box::new(m20260411_000000_rename_server_to_build_machine::Migration),
            Box::new(m20260412_000000_add_superuser_to_user::Migration),
            Box::new(m20260412_000001_create_worker_registration::Migration),
            Box::new(m20260412_000002_convert_architecture_to_string::Migration),
            Box::new(m20260412_000003_drop_ssh_server_tables::Migration),
            Box::new(m20260412_000004_add_managed_to_worker_registration::Migration),
            Box::new(m20260413_000000_add_url_to_worker_registration::Migration),
            Box::new(m20260414_000000_add_active_to_worker_registration::Migration),
            Box::new(m20260416_000000_create_table_cached_path::Migration),
            Box::new(m20260418_000000_rename_feature_add_kind::Migration),
            Box::new(m20260418_000001_add_name_to_worker_registration::Migration),
            Box::new(m20260420_000000_add_flake_source_to_evaluation::Migration),
            Box::new(m20260421_000000_create_table_integration::Migration),
            Box::new(m20260421_000002_drop_use_nix_store_from_organization::Migration),
            Box::new(m20260421_000003_create_table_build_product::Migration),
            Box::new(m20260422_000000_rename_worker_name_and_add_created_by::Migration),
            Box::new(m20260422_000001_add_display_name_to_integration::Migration),
            Box::new(m20260422_000002_add_enable_caps_to_worker_registration::Migration),
            Box::new(m20260422_000003_add_deriver_to_cached_path::Migration),
            Box::new(m20260425_000000_replace_build_server_with_worker::Migration),
            Box::new(m20260430_000000_normalize_hash_columns::Migration),
            Box::new(m20260501_000001_add_repo_check_id::Migration),
            Box::new(m20260502_000000_hash_api_keys::Migration),
            Box::new(m20260502_000001_drop_file_columns_from_derivation_output::Migration),
            Box::new(m20260502_000002_add_oidc_identity_to_user::Migration),
            Box::new(m20260504_000000_cached_path_signature_to_bytea::Migration),
            Box::new(m20260504_000001_add_via_to_build::Migration),
            Box::new(m20260504_000002_add_external_cached_to_build::Migration),
            Box::new(m20260505_000000_index_build_dispatch::Migration),
            Box::new(m20260505_000001_api_key_lifecycle::Migration),
            Box::new(m20260505_000002_create_table_session::Migration),
            Box::new(m20260505_000003_create_table_audit_log::Migration),
            Box::new(m20260505_000004_create_table_webhook_delivery::Migration),
            Box::new(m20260506_000000_add_waiting_reason_to_evaluation::Migration),
            Box::new(m20260506_000001_index_derivation_output_cache_lookup::Migration),
            Box::new(m20260507_000000_create_table_project_trigger::Migration),
            Box::new(m20260507_000001_add_trigger_to_evaluation::Migration),
            Box::new(m20260507_000002_drop_inbound_integration::Migration),
            Box::new(m20260507_000003_unique_active_evaluation_per_project::Migration),
            Box::new(m20260507_000004_move_concurrency_to_project::Migration),
            Box::new(m20260507_000005_evaluation_concurrent_flag::Migration),
            Box::new(m20260507_000006_add_subtype_to_build_product::Migration),
            Box::new(m20260507_000007_add_sign_cache_to_project::Migration),
            Box::new(m20260508_000000_rename_project_evaluation_wildcard::Migration),
            Box::new(m20260510_000000_seed_github_app_integrations::Migration),
            Box::new(m20260510_000001_api_key_options::Migration),
            Box::new(m20260510_000002_add_managed_to_role::Migration),
            Box::new(m20260511_000000_strip_derivation_path_prefix::Migration),
            Box::new(m20260513_000000_add_local_priority_to_cache::Migration),
            Box::new(m20260519_000000_normalize_derivation_columns::Migration),
            Box::new(m20260519_000001_create_build_request_blob::Migration),
            Box::new(m20260519_000002_create_upload_session::Migration),
            Box::new(m20260519_000003_add_organization_hide_build_requests::Migration),
            Box::new(m20260519_000004_drop_direct_build::Migration),
            Box::new(m20260521_000000_tag_waiting_reason_kind::Migration),
            Box::new(m20260522_000000_cached_path_signature_fetch_stats::Migration),
            Box::new(m20260522_000001_create_flake_input_override::Migration),
            Box::new(m20260524_000000_create_table_project_action::Migration),
            Box::new(m20260524_000001_create_table_project_action_delivery::Migration),
            Box::new(m20260524_000002_drop_table_webhook_delivery::Migration),
            Box::new(m20260524_000003_drop_table_webhook::Migration),
            Box::new(m20260524_000004_drop_table_project_integration::Migration),
            Box::new(m20260525_000000_create_table_cache_role::Migration),
            Box::new(m20260525_000001_create_table_cache_user::Migration),
            Box::new(m20260525_000002_add_cache_to_api::Migration),
            Box::new(m20260525_000003_create_admin_task::Migration),
            Box::new(m20260525_000004_evaluation_check_run_ids::Migration),
            Box::new(m20260527_000000_evaluation_source_comment::Migration),
            Box::new(m20260527_000001_add_allowed_ips_to_api::Migration),
            Box::new(m20260527_000002_add_allowed_ips_to_integration::Migration),
            Box::new(m20260528_000000_create_table_cli_device_authorization::Migration),
            Box::new(m20260529_000000_add_cache_upstream_kind::Migration),
            Box::new(m20260603_000000_add_build_failure_retry_fields::Migration),
            Box::new(m20260604_000001_add_derivation_scoring_columns::Migration),
            Box::new(m20260604_000002_create_derivation_metric::Migration),
            Box::new(m20260605_000000_index_hot_lookups::Migration),
            Box::new(m20260605_000001_create_build_log_chunk::Migration),
            Box::new(m20260606_000000_add_derivation_fixed_output::Migration),
            Box::new(m20260607_000000_add_max_storage_to_cache::Migration),
            Box::new(m20260607_000001_create_table_phase_event::Migration),
            Box::new(m20260607_000002_create_table_dispatched_job::Migration),
            Box::new(m20260607_000003_create_table_worker_sample::Migration),
            Box::new(m20260607_000004_create_table_worker_connection::Migration),
            Box::new(m20260607_000005_create_table_acknowledged_derivation::Migration),
            Box::new(m20260607_000006_create_table_metric_rollup::Migration),
            Box::new(m20260607_000007_add_phase_timing_to_build::Migration),
            Box::new(m20260607_000008_add_phase_timing_to_evaluation::Migration),
            Box::new(m20260607_000009_add_queued_at_to_build::Migration),
            Box::new(m20260607_000010_add_network_to_derivation_metric::Migration),
            Box::new(m20260608_000001_add_dispatched_job_instance_context::Migration),
            Box::new(m20260610_000001_create_build_attempt::Migration),
            Box::new(m20260610_000002_backfill_build_attempt::Migration),
            Box::new(m20260610_000003_slim_build_and_dispatched_job::Migration),
            Box::new(m20260612_000001_add_started_by_to_evaluation::Migration),
            Box::new(m20260613_000001_add_dep_closure_counts::Migration),
            Box::new(m20260614_000001_eval_cache_store::Migration),
            Box::new(m20260614_000002_add_scim_to_user::Migration),
            Box::new(m20260615_000001_create_evaluation_metric::Migration),
            Box::new(m20260615_000002_create_evaluation_attr_cost::Migration),
            Box::new(m20260615_000003_create_flake_output_node::Migration),
            Box::new(m20260615_000004_create_base_worker::Migration),
            Box::new(m20260615_000005_create_organization_base_worker::Migration),
            Box::new(m20260616_000001_add_kind_to_evaluation::Migration),
            Box::new(m20260616_000002_create_evaluation_input_update::Migration),
            Box::new(m20260616_000003_create_open_pr_state::Migration),
            Box::new(m20260617_000001_add_substituted_to_build::Migration),
            Box::new(m20260619_000001_drop_cached_path_store_path::Migration),
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
        ]
    }
}
