/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `cached_path.confirmed`: the object is durably in `nar_storage`. It is false
//! only between a relayed commit on the S3 backend and the uploader's confirm,
//! so every row that exists today is confirmed by the default, and the scan the
//! uploader runs is a partial index that is empty on a quiet server.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    "ALTER TABLE cached_path ADD COLUMN IF NOT EXISTS confirmed boolean NOT NULL DEFAULT true",
    "CREATE INDEX IF NOT EXISTS \"idx-cached_path-unconfirmed\" ON cached_path (created_at) WHERE NOT confirmed",
];

const DOWN: &[&str] = &[
    "DROP INDEX IF EXISTS \"idx-cached_path-unconfirmed\"",
    "ALTER TABLE cached_path DROP COLUMN IF EXISTS confirmed",
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
