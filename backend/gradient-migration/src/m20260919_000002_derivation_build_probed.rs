/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Defaulting to true is leaving every existing shared build as the deploy found it. The ingest is
//! writing false for new shared builds.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    "ALTER TABLE derivation_build ADD COLUMN IF NOT EXISTS probed boolean NOT NULL DEFAULT true",
    "CREATE INDEX IF NOT EXISTS \"idx-derivation_build-unprobed\" \
     ON derivation_build (derivation) WHERE NOT probed AND demanded",
];

const DOWN: &[&str] = &[
    "DROP INDEX IF EXISTS \"idx-derivation_build-unprobed\"",
    "ALTER TABLE derivation_build DROP COLUMN IF EXISTS probed",
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
