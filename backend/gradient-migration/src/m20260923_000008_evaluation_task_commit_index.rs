/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The dashboard reads a task's newest evaluations and finds evaluations by commit;
//! without these both sequentially scan `evaluation` once per task or per search.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    r#"CREATE INDEX IF NOT EXISTS "idx-evaluation-task-created_at"
     ON evaluation (task, created_at DESC)"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-evaluation-commit" ON evaluation (commit)"#,
];

const DOWN: &[&str] = &[
    r#"DROP INDEX IF EXISTS "idx-evaluation-commit""#,
    r#"DROP INDEX IF EXISTS "idx-evaluation-task-created_at""#,
];

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
