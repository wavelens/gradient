/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
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
        ]
    }
}
