/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! PR lifecycle for the `OpenPr` action: one row per (project, action, branch).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();

        conn.execute_unprepared(
            r#"
            CREATE TABLE open_pr_state (
                id UUID PRIMARY KEY,
                project UUID NOT NULL REFERENCES project (id) ON DELETE CASCADE,
                action UUID NOT NULL REFERENCES project_action (id) ON DELETE CASCADE,
                branch TEXT NOT NULL,
                forge_pr_number BIGINT NULL,
                head_commit TEXT NULL,
                status TEXT NOT NULL,
                created_at TIMESTAMP NOT NULL,
                updated_at TIMESTAMP NOT NULL
            )
            "#,
        )
        .await?;

        conn.execute_unprepared(
            r#"
            CREATE UNIQUE INDEX "uq-open_pr_state-project-action-branch"
              ON open_pr_state (project, action, branch)
            "#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(r#"DROP INDEX IF EXISTS "uq-open_pr_state-project-action-branch""#)
            .await?;
        conn.execute_unprepared(r#"DROP TABLE IF EXISTS open_pr_state"#).await?;

        Ok(())
    }
}
