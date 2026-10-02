/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `unwalked_inputs` is deliberately not backfilled. The consistency sweep's recount is the
//! backfill, and until then every walked row is reading as complete.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] =
    &["ALTER TABLE derivation ADD COLUMN IF NOT EXISTS unwalked_inputs integer NOT NULL DEFAULT 0"];

const DOWN: &[&str] = &["ALTER TABLE derivation DROP COLUMN IF EXISTS unwalked_inputs"];

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
