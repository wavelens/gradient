/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Adds `evaluation.concurrent` (default false) and rebuilds the
//! "one active eval per project" partial unique index to skip rows
//! that opted into concurrent evaluations via the `all` policy.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE evaluation ADD COLUMN IF NOT EXISTS concurrent boolean NOT NULL DEFAULT false",
        ).await?;
        db.execute_unprepared("DROP INDEX IF EXISTS uq_evaluation_one_active_per_project")
            .await?;
        db.execute_unprepared(
            r#"CREATE UNIQUE INDEX uq_evaluation_one_active_per_project
               ON evaluation(project)
               WHERE project IS NOT NULL
                 AND status IN (0,1,2,3,4,8)
                 AND NOT concurrent"#,
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP INDEX IF EXISTS uq_evaluation_one_active_per_project")
            .await?;
        db.execute_unprepared(
            r#"CREATE UNIQUE INDEX uq_evaluation_one_active_per_project
               ON evaluation(project)
               WHERE project IS NOT NULL
                 AND status IN (0,1,2,3,4,8)"#,
        )
        .await?;
        db.execute_unprepared("ALTER TABLE evaluation DROP COLUMN IF EXISTS concurrent")
            .await?;
        Ok(())
    }
}
