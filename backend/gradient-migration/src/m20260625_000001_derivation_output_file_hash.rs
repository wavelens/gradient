/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Persist the upstream narinfo `FileHash` (compressed-NAR hash) on
//! `derivation_output`. Lets the worker relay a substitutable NAR verbatim -
//! reporting the upstream file hash instead of recomputing it - when the
//! upstream payload is already zstd-compressed at our window threshold.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE derivation_output
                   ADD COLUMN IF NOT EXISTS file_hash text",
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE derivation_output
                   DROP COLUMN IF EXISTS file_hash",
            )
            .await?;

        Ok(())
    }
}
