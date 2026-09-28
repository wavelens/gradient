/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Per-user stars on projects, tasks and caches; real foreign keys so deletes cascade.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    r#"CREATE TABLE IF NOT EXISTS user_project_star (
        "user" uuid NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
        project uuid NOT NULL REFERENCES project(id) ON DELETE CASCADE,
        created_at timestamp NOT NULL DEFAULT now(),
        PRIMARY KEY ("user", project))"#,
    r#"CREATE TABLE IF NOT EXISTS user_task_star (
        "user" uuid NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
        task uuid NOT NULL REFERENCES task(id) ON DELETE CASCADE,
        created_at timestamp NOT NULL DEFAULT now(),
        PRIMARY KEY ("user", task))"#,
    r#"CREATE TABLE IF NOT EXISTS user_cache_star (
        "user" uuid NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
        cache uuid NOT NULL REFERENCES cache(id) ON DELETE CASCADE,
        created_at timestamp NOT NULL DEFAULT now(),
        PRIMARY KEY ("user", cache))"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-user_project_star-project" ON user_project_star (project)"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-user_task_star-task" ON user_task_star (task)"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-user_cache_star-cache" ON user_cache_star (cache)"#,
];

const DOWN: &[&str] = &[
    "DROP TABLE IF EXISTS user_cache_star",
    "DROP TABLE IF EXISTS user_task_star",
    "DROP TABLE IF EXISTS user_project_star",
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
