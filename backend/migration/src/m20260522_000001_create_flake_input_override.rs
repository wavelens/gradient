/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Per-project flake input overrides + per-evaluation snapshots.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();

        conn.execute_unprepared(
            r#"
            CREATE TABLE project_flake_input_override (
                id UUID PRIMARY KEY,
                project UUID NOT NULL REFERENCES project (id) ON DELETE CASCADE,
                input_name TEXT NOT NULL,
                url TEXT NULL,
                created_at TIMESTAMP NOT NULL,
                updated_at TIMESTAMP NOT NULL
            )
            "#,
        )
        .await?;

        conn.execute_unprepared(
            r#"
            CREATE UNIQUE INDEX "uq-project_flake_input_override-project-input_name"
              ON project_flake_input_override (project, input_name)
            "#,
        )
        .await?;

        conn.execute_unprepared(
            r#"
            CREATE TABLE evaluation_flake_input_override (
                id UUID PRIMARY KEY,
                evaluation UUID NOT NULL REFERENCES evaluation (id) ON DELETE CASCADE,
                input_name TEXT NOT NULL,
                url TEXT NULL
            )
            "#,
        )
        .await?;

        conn.execute_unprepared(
            r#"
            CREATE INDEX "idx-evaluation_flake_input_override-evaluation"
              ON evaluation_flake_input_override (evaluation)
            "#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(
            r#"DROP INDEX IF EXISTS "idx-evaluation_flake_input_override-evaluation""#,
        )
        .await?;
        conn.execute_unprepared(r#"DROP TABLE IF EXISTS evaluation_flake_input_override"#)
            .await?;
        conn.execute_unprepared(
            r#"DROP INDEX IF EXISTS "uq-project_flake_input_override-project-input_name""#,
        )
        .await?;
        conn.execute_unprepared(r#"DROP TABLE IF EXISTS project_flake_input_override"#)
            .await?;
        Ok(())
    }
}
