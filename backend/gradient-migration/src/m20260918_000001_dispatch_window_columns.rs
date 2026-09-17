/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The instance metrics pass averages nine things over the last 24 hours of
//! dispatches, every 30 seconds. Three of them lived only inside `job_context`,
//! so the pass read every row in the window out of the heap to pull three
//! scalars out of a jsonb: measured in production at 1.94M buffers and 1.6 s
//! against 449k rows, of which the scan itself was 183k buffers.
//!
//! The scalars become columns and the window gets an index that carries them,
//! so the aggregate is an index-only scan over the window rather than a
//! sequential scan of the table. Deliberately no backfill: the columns are
//! written from the same view that writes the jsonb, `AVG` skips nulls exactly
//! as it skipped an absent json key, and every window is at most 24 hours long,
//! so the averages cover the whole window again one day after deploy.
//!
//! `build_attempt` gets the index its `DISTINCT ON (derivation_build) ... ORDER
//! BY derivation_build, created_at DESC` has always wanted; without it every
//! lookup of "the latest attempt per anchor" sorts what it read.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    "ALTER TABLE dispatched_job \
     ADD COLUMN IF NOT EXISTS missing_nar_size bigint, \
     ADD COLUMN IF NOT EXISTS missing_count integer, \
     ADD COLUMN IF NOT EXISTS dependency_count integer",
    "CREATE INDEX IF NOT EXISTS \"idx-dispatched_job-build-window\" \
     ON dispatched_job (dispatched_at) \
     INCLUDE (ready_at, missing_nar_size, missing_count, dependency_count) \
     WHERE kind = 1 AND ready_at IS NOT NULL",
    "CREATE INDEX IF NOT EXISTS \"idx-build_attempt-latest\" \
     ON build_attempt (derivation_build, created_at DESC)",
];

const DOWN: &[&str] = &[
    "DROP INDEX IF EXISTS \"idx-build_attempt-latest\"",
    "DROP INDEX IF EXISTS \"idx-dispatched_job-build-window\"",
    "ALTER TABLE dispatched_job \
     DROP COLUMN IF EXISTS missing_nar_size, \
     DROP COLUMN IF EXISTS missing_count, \
     DROP COLUMN IF EXISTS dependency_count",
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

    /// The window index only helps if it carries every column the aggregate
    /// reads: one column left out of `INCLUDE` puts the heap fetch back and the
    /// index-only scan with it.
    #[test]
    fn the_window_index_covers_what_the_aggregate_reads() {
        let index = UP[1];
        for column in [
            "ready_at",
            "missing_nar_size",
            "missing_count",
            "dependency_count",
        ] {
            assert!(index.contains(column), "{column} is not covered: {index}");
        }
        assert!(
            index.contains("WHERE kind = 1 AND ready_at IS NOT NULL"),
            "the predicate is the one the statement spells, or Postgres cannot \
             prove the implication: {index}"
        );
    }

    /// `DISTINCT ON (derivation_build) ... ORDER BY derivation_build, created_at
    /// DESC` reads the index in its own order, so the descending key is what
    /// removes the sort rather than just the lookup.
    #[test]
    fn the_attempt_index_is_ordered_the_way_the_lookup_reads_it() {
        assert!(
            UP[2].contains("build_attempt (derivation_build, created_at DESC)"),
            "{}",
            UP[2]
        );
    }
}
