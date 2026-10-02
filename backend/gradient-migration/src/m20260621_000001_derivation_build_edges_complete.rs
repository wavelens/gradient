/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The backfill is skipping `Created`, never-dispatched shared builds without edges. Those rows
//! were stranded by an incomplete eval and must wait for a complete eval to flush their graph.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(
            "ALTER TABLE derivation_build
               ADD COLUMN IF NOT EXISTS edges_complete boolean NOT NULL DEFAULT false",
        )
        .await?;

        conn.execute_unprepared(
            "UPDATE derivation_build db SET edges_complete = true
             WHERE db.status <> 0
                OR db.dispatched_at IS NOT NULL
                OR EXISTS (SELECT 1 FROM derivation_dependency e
                           WHERE e.derivation = db.derivation)",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE derivation_build DROP COLUMN IF EXISTS edges_complete")
            .await?;

        Ok(())
    }
}
