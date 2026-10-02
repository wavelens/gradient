/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"CREATE INDEX IF NOT EXISTS "idx-build_job-derivation-evaluation"
               ON build_job (derivation) INCLUDE (evaluation)"#,
        )
        .await?;

        db.execute_unprepared("DROP INDEX IF EXISTS idx_build_job_derivation")
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_build_job_derivation ON build_job (derivation)",
        )
        .await?;

        db.execute_unprepared(r#"DROP INDEX IF EXISTS "idx-build_job-derivation-evaluation""#)
            .await?;

        Ok(())
    }
}
