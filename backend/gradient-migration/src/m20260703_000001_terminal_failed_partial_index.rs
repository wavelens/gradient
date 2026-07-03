/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Partial index for the proactive dependency-failed sweep and the requeue
//! paths, which seed recursive walks from the terminal-failed anchors on every
//! dispatch tick (`status IN (FailedPermanent=4, DependencyFailed=6,
//! FailedTimeout=9)`, requeue additionally Aborted=5).

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
                r#"CREATE INDEX IF NOT EXISTS "idx-derivation_build-terminal-failed"
                   ON derivation_build (derivation)
                   WHERE status IN (4, 5, 6, 9)"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(r#"DROP INDEX IF EXISTS "idx-derivation_build-terminal-failed""#)
            .await?;
        Ok(())
    }
}
