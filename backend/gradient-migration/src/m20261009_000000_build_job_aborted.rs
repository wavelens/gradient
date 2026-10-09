/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Recount the live counters under the new function. Without it, the trigger deltas drift from
//! the stored totals.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const ADD_COLUMN: &str = "ALTER TABLE build_job ADD COLUMN aborted boolean NOT NULL DEFAULT false";

const DROP_COLUMN: &str = "ALTER TABLE build_job DROP COLUMN aborted";

pub const SHARED_BUILD_COUNTS_FN: &str = "CREATE OR REPLACE FUNCTION evaluation_shared_build_counts(\
     status integer, wanted boolean, \
     OUT active integer, OUT failed integer, OUT queued integer, OUT building integer) \
     LANGUAGE sql IMMUTABLE AS $$ SELECT \
     ((evaluation_shared_build_counts.status IN (0, 1, 2, 8, 10) AND (evaluation_shared_build_counts.wanted OR evaluation_shared_build_counts.status IN (1, 2))))::int, \
     (evaluation_shared_build_counts.status IN (4, 5, 6, 9))::int, \
     (evaluation_shared_build_counts.status = 1)::int, \
     (evaluation_shared_build_counts.status = 2)::int $$";

const RECOUNT_LIVE: &str = "WITH live AS (SELECT id FROM evaluation WHERE status IN (0, 8, 1, 2, 3, 4)), \
     gone AS (DELETE FROM evaluation_shared_build_delta d USING live WHERE d.evaluation = live.id), \
     c AS (SELECT e.id, count(bj.id)::int AS named, coalesce(sum(x.active), 0)::int AS active, \
           coalesce(sum(x.failed), 0)::int AS failed, coalesce(sum(x.queued), 0)::int AS queued, \
           coalesce(sum(x.building), 0)::int AS building \
           FROM live e LEFT JOIN build_job bj ON bj.evaluation = e.id \
           LEFT JOIN derivation_build db ON db.id = bj.derivation_build \
           LEFT JOIN LATERAL evaluation_shared_build_counts(db.status, db.wanted) x ON db.id IS NOT NULL \
           GROUP BY e.id) \
     UPDATE evaluation e SET named_shared_builds = c.named, active_shared_builds = c.active, \
     failed_shared_builds = c.failed, queued_shared_builds = c.queued, building_shared_builds = c.building \
     FROM c WHERE e.id = c.id";

fn previous_counts_fn() -> String {
    SHARED_BUILD_COUNTS_FN.replace("IN (0, 1, 2, 8, 10)", "IN (0, 1, 2, 8, 10, 5)")
}

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(ADD_COLUMN).await?;
        db.execute_unprepared(SHARED_BUILD_COUNTS_FN).await?;
        db.execute_unprepared(RECOUNT_LIVE).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(&previous_counts_fn()).await?;
        db.execute_unprepared(RECOUNT_LIVE).await?;
        db.execute_unprepared(DROP_COLUMN).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_down_migration_restores_the_previous_counts_function() {
        assert_eq!(
            previous_counts_fn(),
            crate::m20261001_000001_plain_concept_names::SHARED_BUILD_COUNTS_FN
        );
    }
}
