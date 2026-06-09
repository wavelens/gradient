/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260412_000003_drop_ssh_server_tables"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop dependent tables first (foreign keys), then parent tables.
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                DROP TABLE IF EXISTS server_feature CASCADE;
                DROP TABLE IF EXISTS server_architecture CASCADE;
                DROP TABLE IF EXISTS build_machine_feature CASCADE;
                DROP TABLE IF EXISTS build_machine_architecture CASCADE;
                DROP TABLE IF EXISTS build_machine CASCADE;
                DROP TABLE IF EXISTS server CASCADE;
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Intentionally not restoring SSH server tables.
        Ok(())
    }
}
