/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    "CREATE INDEX IF NOT EXISTS \"idx-derivation_build-active\" \
     ON derivation_build (derivation) WHERE status IN (0, 1, 2, 8)",
    "CREATE INDEX IF NOT EXISTS \"idx-build_attempt-deterministic-failure\" \
     ON build_attempt (derivation_build) WHERE outcome = 3 AND reason = 5",
];

const DOWN: &[&str] = &[
    "DROP INDEX IF EXISTS \"idx-derivation_build-active\"",
    "DROP INDEX IF EXISTS \"idx-build_attempt-deterministic-failure\"",
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
