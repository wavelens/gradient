/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Upstream-availability columns on `derivation_output`. Resolved once at eval
//! via the org's upstream-cache narinfo lookup: `external_url` is the upstream
//! NAR URL, the rest is the narinfo metadata the worker needs to download and
//! import the path directly (no second narinfo fetch). When set, the anchor is
//! dispatched substitutable; `cached_path` stays empty until the worker pulls,
//! recompresses, and pushes the NAR into the gradient cache.

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
                   ADD COLUMN IF NOT EXISTS external_url text,
                   ADD COLUMN IF NOT EXISTS nar_hash text,
                   ADD COLUMN IF NOT EXISTS file_size bigint,
                   ADD COLUMN IF NOT EXISTS references_list text,
                   ADD COLUMN IF NOT EXISTS deriver text",
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE derivation_output
                   DROP COLUMN IF EXISTS external_url,
                   DROP COLUMN IF EXISTS nar_hash,
                   DROP COLUMN IF EXISTS file_size,
                   DROP COLUMN IF EXISTS references_list,
                   DROP COLUMN IF EXISTS deriver",
            )
            .await?;

        Ok(())
    }
}
