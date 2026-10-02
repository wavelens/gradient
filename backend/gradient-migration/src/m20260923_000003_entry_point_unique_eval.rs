/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    "DELETE FROM entry_point a USING entry_point b \
     WHERE a.evaluation = b.evaluation AND a.eval = b.eval AND a.id > b.id",
    r#"DROP INDEX IF EXISTS "idx-entry_point-evaluation-eval""#,
    r#"CREATE UNIQUE INDEX IF NOT EXISTS "idx-entry_point-evaluation-eval"
       ON entry_point (evaluation, eval)"#,
];

const DOWN: &[&str] = &[
    r#"DROP INDEX IF EXISTS "idx-entry_point-evaluation-eval""#,
    r#"CREATE INDEX IF NOT EXISTS "idx-entry_point-evaluation-eval"
       ON entry_point (evaluation, eval, id)"#,
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
    use super::UP;

    #[test]
    fn duplicates_are_gone_before_the_index_is_built() {
        assert!(UP[0].starts_with("DELETE FROM entry_point"), "{}", UP[0]);
    }
}
