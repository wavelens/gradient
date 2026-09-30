/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The cached-anchor reconcile settles the anchors of an evaluation's closure that
//! are not yet `Completed` or `Substituted`. Those are a few percent of the table,
//! and without an index to name them the planner scanned every anchor to find them.

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
                r#"CREATE INDEX IF NOT EXISTS "idx-derivation_build-unsettled"
                   ON derivation_build (derivation)
                   WHERE status <> ALL (ARRAY[3, 7])"#,
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(r#"DROP INDEX IF EXISTS "idx-derivation_build-unsettled""#)
            .await?;

        Ok(())
    }
}
