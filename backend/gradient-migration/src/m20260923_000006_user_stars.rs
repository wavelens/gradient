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

#[cfg(test)]
mod tests {
    use super::{DOWN, UP};

    #[test]
    fn every_star_cascades_from_both_sides() {
        for table in &UP[..3] {
            assert_eq!(table.matches("ON DELETE CASCADE").count(), 2, "{table}");
        }
    }

    #[test]
    fn a_star_is_unique_per_user_and_target() {
        for (table, target) in UP[..3].iter().zip(["project", "task", "cache"]) {
            assert!(
                table.contains(&format!(r#"PRIMARY KEY ("user", {target})"#)),
                "{table}"
            );
        }
    }

    #[test]
    fn down_drops_every_table_up_creates() {
        for name in ["user_project_star", "user_task_star", "user_cache_star"] {
            assert!(DOWN.iter().any(|d| d.ends_with(name)), "{name}");
        }
    }
}
