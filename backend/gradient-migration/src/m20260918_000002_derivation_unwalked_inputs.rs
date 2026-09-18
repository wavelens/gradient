/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The walk prunes at a derivation whose subtree is recorded. `walked` only says
//! the derivation's own record is written, and a walk that dies between batches
//! leaves it true above inputs it merely named as stubs; every later walk then
//! prunes there and the stubs stay stubs (11,071 in production on 2026-09-18,
//! 5,450 of them carrying a build_job nothing can dispatch).
//!
//! `unwalked_inputs` counts the direct inputs whose subtree is not recorded, seeded
//! when the record lands and counted down by the ingest ripple. Deliberately no
//! backfill: the consistency sweep's recount is the backfill, exactly as it is for
//! `demanded`, and until it runs every walked row reads complete, which is what the
//! prune assumed before this column existed.

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
