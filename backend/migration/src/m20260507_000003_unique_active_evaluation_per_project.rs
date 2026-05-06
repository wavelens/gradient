/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Partial unique index: at most one active evaluation per project.
        // Active statuses: 0=Queued, 1=EvaluatingFlake, 2=EvaluatingDerivation,
        // 3=Building, 4=Waiting, 8=Fetching. (5=Completed, 6=Failed, 7=Aborted are terminal.)
        manager
            .get_connection()
            .execute_unprepared(
                r#"CREATE UNIQUE INDEX IF NOT EXISTS "uq_evaluation_one_active_per_project"
                   ON evaluation(project)
                   WHERE project IS NOT NULL AND status IN (0,1,2,3,4,8)"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"DROP INDEX IF EXISTS "uq_evaluation_one_active_per_project""#,
            )
            .await?;
        Ok(())
    }
}
