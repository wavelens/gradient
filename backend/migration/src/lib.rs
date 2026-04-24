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
mod m20241107_155000_create_table_build_depencdency;
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
mod m20260330_000000_add_has_artefacts_to_build_output;
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
mod m20260421_000001_add_github_app_enabled_to_organization;
mod m20260421_000002_drop_use_nix_store_from_organization;
mod m20260421_000003_create_table_build_product;
mod m20260421_000004_drop_has_artefacts_from_derivation_output;
mod m20260422_000000_rename_worker_name_and_add_created_by;
mod m20260422_000001_add_display_name_to_integration;
mod m20260422_000002_add_enable_caps_to_worker_registration;
mod m20260422_000003_add_deriver_to_cached_path;
mod m20260425_000000_replace_build_server_with_worker;

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
            Box::new(m20241107_155000_create_table_build_depencdency::Migration),
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
            Box::new(m20260330_000000_add_has_artefacts_to_build_output::Migration),
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
            Box::new(m20260421_000001_add_github_app_enabled_to_organization::Migration),
            Box::new(m20260421_000002_drop_use_nix_store_from_organization::Migration),
            Box::new(m20260421_000003_create_table_build_product::Migration),
            Box::new(m20260421_000004_drop_has_artefacts_from_derivation_output::Migration),
            Box::new(m20260422_000000_rename_worker_name_and_add_created_by::Migration),
            Box::new(m20260422_000001_add_display_name_to_integration::Migration),
            Box::new(m20260422_000002_add_enable_caps_to_worker_registration::Migration),
            Box::new(m20260422_000003_add_deriver_to_cached_path::Migration),
            Box::new(m20260425_000000_replace_build_server_with_worker::Migration),
        ]
    }
}
