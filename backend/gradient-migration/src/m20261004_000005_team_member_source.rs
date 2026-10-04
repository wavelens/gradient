/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: [&str; 3] = [
    "ALTER TABLE team_user ADD COLUMN source smallint NOT NULL DEFAULT 0",
    "UPDATE team_user SET source = 2 WHERE via_group",
    "ALTER TABLE team_user DROP COLUMN via_group",
];

const DOWN: [&str; 3] = [
    "ALTER TABLE team_user ADD COLUMN via_group boolean NOT NULL DEFAULT false",
    "UPDATE team_user SET via_group = true WHERE source = 2",
    "ALTER TABLE team_user DROP COLUMN source",
];

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
        for statement in DOWN {
            manager
                .get_connection()
                .execute_unprepared(statement)
                .await?;
        }
        Ok(())
    }
}
