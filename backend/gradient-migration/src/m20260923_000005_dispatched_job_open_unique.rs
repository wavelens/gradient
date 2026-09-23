/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! A job is out at most once. The claim inserts its `dispatched_job` row with
//! `ON CONFLICT (job_id) WHERE finished_at IS NULL DO NOTHING`, so two
//! instances racing for one job cannot both win; the newest open row of each
//! job survives and older duplicates close as abandoned (outcome 2).

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    "UPDATE dispatched_job SET finished_at = now() AT TIME ZONE 'UTC', outcome = 2 \
     WHERE finished_at IS NULL AND job_id IS NOT NULL AND id NOT IN ( \
       SELECT DISTINCT ON (job_id) id FROM dispatched_job \
       WHERE finished_at IS NULL AND job_id IS NOT NULL \
       ORDER BY job_id, dispatched_at DESC)",
    r#"CREATE UNIQUE INDEX IF NOT EXISTS "idx-dispatched_job-open-job"
       ON dispatched_job (job_id) WHERE finished_at IS NULL"#,
    r#"DROP INDEX IF EXISTS "idx-dispatched_job-open-by-job-id""#,
];

const DOWN: &[&str] = &[
    r#"CREATE INDEX IF NOT EXISTS "idx-dispatched_job-open-by-job-id"
       ON dispatched_job (job_id, dispatched_at DESC) WHERE finished_at IS NULL"#,
    r#"DROP INDEX IF EXISTS "idx-dispatched_job-open-job""#,
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
    fn duplicates_close_before_the_index_is_built() {
        assert!(UP[0].starts_with("UPDATE dispatched_job"), "{}", UP[0]);
        assert!(UP[0].contains("outcome = 2"), "{}", UP[0]);
    }

    /// The claim's `ON CONFLICT` names exactly this column and predicate.
    #[test]
    fn the_index_is_unique_on_the_open_job_key() {
        assert!(
            UP[1].contains("CREATE UNIQUE INDEX")
                && UP[1].contains("(job_id) WHERE finished_at IS NULL"),
            "{}",
            UP[1]
        );
    }
}
