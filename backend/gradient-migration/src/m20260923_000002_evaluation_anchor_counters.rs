/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Per-evaluation anchor counters. Triggers append signed deltas to an
//! append-only ledger so no mover ever locks the evaluation row; the dispatch
//! tick folds the ledger, and the consistency sweep recounts.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

pub const ANCHOR_COUNTS_FN: &str = "CREATE OR REPLACE FUNCTION evaluation_anchor_counts(\
     status integer, demanded boolean, \
     OUT active integer, OUT failed integer, OUT queued integer, OUT building integer) \
     LANGUAGE sql IMMUTABLE AS $$ SELECT \
     ((evaluation_anchor_counts.status IN (0, 1, 2, 8, 10, 5) AND (evaluation_anchor_counts.demanded OR evaluation_anchor_counts.status IN (1, 2))))::int, \
     (evaluation_anchor_counts.status IN (4, 5, 6, 9))::int, \
     (evaluation_anchor_counts.status = 1)::int, \
     (evaluation_anchor_counts.status = 2)::int $$";

const MOVED_FN: &str = "CREATE OR REPLACE FUNCTION evaluation_anchor_moved() RETURNS trigger \
     LANGUAGE plpgsql AS $$ BEGIN \
     INSERT INTO evaluation_anchor_delta (evaluation, named, active, failed, queued, building) \
     SELECT bj.evaluation, 0, sum(d.active)::int, sum(d.failed)::int, sum(d.queued)::int, sum(d.building)::int \
     FROM ( \
       SELECT n.id, nc.active - oc.active AS active, nc.failed - oc.failed AS failed, \
              nc.queued - oc.queued AS queued, nc.building - oc.building AS building \
       FROM new_rows n JOIN old_rows o ON o.id = n.id \
       CROSS JOIN LATERAL evaluation_anchor_counts(n.status, n.demanded) nc \
       CROSS JOIN LATERAL evaluation_anchor_counts(o.status, o.demanded) oc \
       WHERE n.status IS DISTINCT FROM o.status OR n.demanded IS DISTINCT FROM o.demanded \
     ) d \
     JOIN build_job bj ON bj.derivation_build = d.id \
     WHERE d.active <> 0 OR d.failed <> 0 OR d.queued <> 0 OR d.building <> 0 \
     GROUP BY bj.evaluation; \
     RETURN NULL; END $$";

const NAMED_FN: &str = "CREATE OR REPLACE FUNCTION evaluation_anchor_named() RETURNS trigger \
     LANGUAGE plpgsql AS $$ BEGIN \
     PERFORM 1 FROM derivation_build db \
       WHERE db.id IN (SELECT derivation_build FROM new_rows) \
       ORDER BY db.derivation FOR SHARE; \
     INSERT INTO evaluation_anchor_delta (evaluation, named, active, failed, queued, building) \
     SELECT n.evaluation, count(*)::int, sum(c.active)::int, sum(c.failed)::int, sum(c.queued)::int, sum(c.building)::int \
     FROM new_rows n JOIN derivation_build db ON db.id = n.derivation_build \
     CROSS JOIN LATERAL evaluation_anchor_counts(db.status, db.demanded) c \
     GROUP BY n.evaluation; \
     RETURN NULL; END $$";

const UNNAMED_FN: &str = "CREATE OR REPLACE FUNCTION evaluation_anchor_unnamed() RETURNS trigger \
     LANGUAGE plpgsql AS $$ BEGIN \
     PERFORM 1 FROM derivation_build db \
       WHERE db.id IN (SELECT derivation_build FROM old_rows) \
       ORDER BY db.derivation FOR SHARE; \
     INSERT INTO evaluation_anchor_delta (evaluation, named, active, failed, queued, building) \
     SELECT o.evaluation, -count(*)::int, -coalesce(sum(c.active), 0)::int, -coalesce(sum(c.failed), 0)::int, \
            -coalesce(sum(c.queued), 0)::int, -coalesce(sum(c.building), 0)::int \
     FROM old_rows o LEFT JOIN derivation_build db ON db.id = o.derivation_build \
     LEFT JOIN LATERAL evaluation_anchor_counts(db.status, db.demanded) c ON db.id IS NOT NULL \
     GROUP BY o.evaluation; \
     RETURN NULL; END $$";

const BACKFILL: &str = "UPDATE evaluation e SET named_anchors = x.named, active_anchors = x.active, \
     failed_anchors = x.failed, queued_anchors = x.queued, building_anchors = x.building \
     FROM ( \
       SELECT bj.evaluation, count(*)::int AS named, sum(c.active)::int AS active, sum(c.failed)::int AS failed, \
              sum(c.queued)::int AS queued, sum(c.building)::int AS building \
       FROM build_job bj JOIN derivation_build db ON db.id = bj.derivation_build \
       CROSS JOIN LATERAL evaluation_anchor_counts(db.status, db.demanded) c \
       GROUP BY bj.evaluation \
     ) x WHERE e.id = x.evaluation";

const UP: &[&str] = &[
    "ALTER TABLE evaluation \
     ADD COLUMN IF NOT EXISTS named_anchors integer NOT NULL DEFAULT 0, \
     ADD COLUMN IF NOT EXISTS active_anchors integer NOT NULL DEFAULT 0, \
     ADD COLUMN IF NOT EXISTS failed_anchors integer NOT NULL DEFAULT 0, \
     ADD COLUMN IF NOT EXISTS queued_anchors integer NOT NULL DEFAULT 0, \
     ADD COLUMN IF NOT EXISTS building_anchors integer NOT NULL DEFAULT 0",
    "CREATE TABLE IF NOT EXISTS evaluation_anchor_delta ( \
     id bigserial PRIMARY KEY, evaluation uuid NOT NULL, \
     named integer NOT NULL, active integer NOT NULL, failed integer NOT NULL, \
     queued integer NOT NULL, building integer NOT NULL)",
    "CREATE INDEX IF NOT EXISTS \"idx-evaluation_anchor_delta-evaluation\" \
     ON evaluation_anchor_delta (evaluation)",
    ANCHOR_COUNTS_FN,
    MOVED_FN,
    NAMED_FN,
    UNNAMED_FN,
    "CREATE TRIGGER evaluation_anchor_moved AFTER UPDATE ON derivation_build \
     REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows \
     FOR EACH STATEMENT EXECUTE FUNCTION evaluation_anchor_moved()",
    "CREATE TRIGGER evaluation_anchor_named AFTER INSERT ON build_job \
     REFERENCING NEW TABLE AS new_rows \
     FOR EACH STATEMENT EXECUTE FUNCTION evaluation_anchor_named()",
    "CREATE TRIGGER evaluation_anchor_unnamed AFTER DELETE ON build_job \
     REFERENCING OLD TABLE AS old_rows \
     FOR EACH STATEMENT EXECUTE FUNCTION evaluation_anchor_unnamed()",
    BACKFILL,
];

const DOWN: &[&str] = &[
    "DROP TRIGGER IF EXISTS evaluation_anchor_unnamed ON build_job",
    "DROP TRIGGER IF EXISTS evaluation_anchor_named ON build_job",
    "DROP TRIGGER IF EXISTS evaluation_anchor_moved ON derivation_build",
    "DROP FUNCTION IF EXISTS evaluation_anchor_unnamed()",
    "DROP FUNCTION IF EXISTS evaluation_anchor_named()",
    "DROP FUNCTION IF EXISTS evaluation_anchor_moved()",
    "DROP FUNCTION IF EXISTS evaluation_anchor_counts(integer, boolean)",
    "DROP TABLE IF EXISTS evaluation_anchor_delta",
    "ALTER TABLE evaluation DROP COLUMN IF EXISTS named_anchors, \
     DROP COLUMN IF EXISTS active_anchors, DROP COLUMN IF EXISTS failed_anchors, \
     DROP COLUMN IF EXISTS queued_anchors, DROP COLUMN IF EXISTS building_anchors",
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
    use super::*;

    /// The backfill runs after the triggers exist and counts through the same
    /// function they call, so the two cannot disagree.
    #[test]
    fn the_backfill_follows_the_triggers_and_uses_their_function() {
        let position = |needle: &str| UP.iter().position(|s| s.contains(needle)).unwrap();
        assert!(
            position("CREATE TRIGGER evaluation_anchor_unnamed")
                < position("UPDATE evaluation e SET")
        );
        assert!(BACKFILL.contains("evaluation_anchor_counts(db.status, db.demanded)"));
    }

    /// Both `build_job` triggers lock the anchors they read in `derivation`
    /// order, the order `LOCK_ANCHORS` takes them in.
    #[test]
    fn the_naming_triggers_lock_in_derivation_order() {
        for f in [NAMED_FN, UNNAMED_FN] {
            assert!(f.contains("ORDER BY db.derivation FOR SHARE"), "{f}");
        }
    }
}
