/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Every graph change bumps `evaluation.graph_version` on each evaluation naming
//! the anchors it moved, and on full pages each bump rewrote all six indexes.
//! Free space on the page lets the bump stay a heap-only update: 987 buffers
//! instead of 2346 for 168 evaluations. Pages written before this take the space
//! as their rows move.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE evaluation SET (fillfactor = 50)")
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE evaluation RESET (fillfactor)")
            .await?;

        Ok(())
    }
}
