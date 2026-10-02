/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    "ALTER TABLE worker_sample DROP COLUMN IF EXISTS project",
    "ALTER TABLE worker_connection DROP COLUMN IF EXISTS project",
    "ALTER TABLE worker_connection DROP COLUMN IF EXISTS display_name",
];

const DOWN: &[&str] = &[
    "DELETE FROM worker_sample",
    "DELETE FROM worker_connection",
    "ALTER TABLE worker_connection ADD COLUMN IF NOT EXISTS display_name varchar NOT NULL",
    "ALTER TABLE worker_connection ADD COLUMN IF NOT EXISTS project uuid NOT NULL",
    "ALTER TABLE worker_sample ADD COLUMN IF NOT EXISTS project uuid NOT NULL",
];

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for stmt in UP {
            manager.get_connection().execute_unprepared(stmt).await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for stmt in DOWN {
            manager.get_connection().execute_unprepared(stmt).await?;
        }

        Ok(())
    }
}
