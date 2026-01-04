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
        ]
    }
}
