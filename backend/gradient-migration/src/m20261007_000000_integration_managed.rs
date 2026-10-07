/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: [&str; 2] = [
    "ALTER TABLE integration ADD COLUMN managed boolean NOT NULL DEFAULT false",
    "UPDATE integration SET managed = true \
     FROM project \
     WHERE integration.project = project.id \
       AND project.managed \
       AND integration.git_host_type <> 3",
];

const DOWN: &str = "ALTER TABLE integration DROP COLUMN managed";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for statement in UP {
            manager
                .get_connection()
                .execute_unprepared(statement)
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;

        Ok(())
    }
}
