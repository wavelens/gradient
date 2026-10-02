/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The predicate is matching FailedPermanent (4), Aborted (5), DependencyFailed (6) and
//! FailedTimeout (9).

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
