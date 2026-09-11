/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `cached_path.missing_references` replaces `closure_complete`: the number of
//! a path's references that are absent, unbacked or themselves not whole.
//!
//! The seed is one walk, not a fixpoint. "Whole" is a least fixpoint over
//! `cached_path_reference` - a path is whole when it is backed and every path it
//! references is whole - so its complement is plain reachability: a path is NOT
//! whole exactly when it is unbacked, absent, or references one that is. One
//! recursive walk upward from the unbacked and the absent names that set, and one
//! grouped `UPDATE` counts each referrer's edges into it. Converging the old flag
//! first cost a full scan of `cached_path` per hop of the deepest closure, twice,
//! and that is what made this migration block the first start for minutes.
//!
//! Nothing here reads `closure_complete`, which is also what makes it idempotent:
//! the reset ahead of the seed clears a diverged value, and a run that died
//! partway leaves nothing a second `up` would trip over.
//!
//! The walk is fenced with `LATERAL (... OFFSET 0)` for the reason every other
//! recursive walk in this codebase is: unfenced, the planner believes the working
//! table is ten times the seed and merge-joins the whole edge table once per
//! iteration. `idx-cached_path_reference-reference_hash` serves the probe.
//!
//! The seed skips every row with no broken reference, so the transaction that
//! then takes ACCESS EXCLUSIVE for the `DROP COLUMN` does not first rewrite the
//! whole table. The partial index over the rows below zero keeps the consistency
//! sweep's negative-counter count off a full scan; it is empty on a healthy cache.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

/// Every hash that is not whole: unbacked, absent from `cached_path` entirely,
/// or a referrer of one that is.
const BROKEN: &str = r#"
    WITH RECURSIVE broken(hash) AS (
            SELECT cp.hash FROM cached_path cp WHERE cp.file_hash IS NULL
          UNION
            SELECT r.reference_hash FROM cached_path_reference r
            LEFT JOIN cached_path dep ON dep.hash = r.reference_hash
            WHERE dep.hash IS NULL
          UNION
            SELECT s.next FROM broken c, LATERAL (
                SELECT r.referrer AS next FROM cached_path_reference r
                WHERE r.reference_hash = c.hash AND r.referrer <> c.hash OFFSET 0) s)
"#;

fn seed() -> String {
    format!(
        "{BROKEN} \
         UPDATE cached_path cp SET missing_references = m.n \
         FROM (SELECT r.referrer AS hash, count(*) AS n FROM cached_path_reference r \
               JOIN broken b ON b.hash = r.reference_hash \
               WHERE r.referrer <> r.reference_hash GROUP BY r.referrer) m \
         WHERE cp.hash = m.hash"
    )
}

fn up_statements() -> Vec<String> {
    vec![
        "ALTER TABLE cached_path ADD COLUMN IF NOT EXISTS missing_references integer NOT NULL DEFAULT 0".into(),
        "UPDATE cached_path SET missing_references = 0 WHERE missing_references <> 0".into(),
        seed(),
        "DROP INDEX IF EXISTS \"idx-cached_path-closure_complete\"".into(),
        "DROP INDEX IF EXISTS \"idx-cached_path-closure_pending\"".into(),
        "ALTER TABLE cached_path DROP COLUMN IF EXISTS closure_complete".into(),
        "CREATE INDEX IF NOT EXISTS \"idx-cached_path-negative_references\" ON cached_path (hash) WHERE missing_references < 0".into(),
    ]
}

const DOWN: &[&str] = &[
    "ALTER TABLE cached_path ADD COLUMN IF NOT EXISTS closure_complete boolean NOT NULL DEFAULT false",
    "UPDATE cached_path SET closure_complete = (file_hash IS NOT NULL AND missing_references = 0)",
    "CREATE INDEX IF NOT EXISTS \"idx-cached_path-closure_complete\" ON cached_path (hash) WHERE closure_complete",
    "CREATE INDEX IF NOT EXISTS \"idx-cached_path-closure_pending\" ON cached_path (hash) WHERE NOT closure_complete AND file_hash IS NOT NULL",
    "DROP INDEX IF EXISTS \"idx-cached_path-negative_references\"",
    "ALTER TABLE cached_path DROP COLUMN IF EXISTS missing_references",
];

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for stmt in up_statements() {
            manager.get_connection().execute_unprepared(&stmt).await?;
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
    use super::{DOWN, seed, up_statements};

    /// The value is re-derived from ground truth, so no statement may READ the
    /// flag it replaces. Only the tail that removes it may name it, and a
    /// `DROP INDEX` names an index rather than a column. `down` restores it and is
    /// exempt. This is also what makes `up` idempotent: the old converge loop read
    /// `closure_complete` and so could not survive its own `DROP COLUMN`.
    #[test]
    fn the_backfill_never_reads_the_flag_it_replaces() {
        let drop_column = "ALTER TABLE cached_path DROP COLUMN IF EXISTS closure_complete";
        for stmt in up_statements() {
            assert!(
                !stmt.contains("closure_complete")
                    || stmt.starts_with("DROP INDEX IF EXISTS")
                    || stmt == drop_column,
                "up reads closure_complete: {stmt}"
            );
        }

        assert!(
            up_statements().iter().any(|s| s == drop_column),
            "up must drop closure_complete once nothing reads it"
        );
        assert!(
            DOWN.iter().any(|s| s.contains("closure_complete")),
            "down must restore the flag it dropped"
        );
    }

    /// One walk, not a fixpoint: a `LOOP` over the whole table per hop is what
    /// this migration was, and the recursive term has to stay fenced or the
    /// planner merge-joins the edge table once per iteration.
    #[test]
    fn the_seed_is_one_fenced_walk() {
        let sql = seed().split_whitespace().collect::<Vec<_>>().join(" ");

        assert!(!sql.contains("LOOP"), "the seed must not iterate: {sql}");
        assert!(sql.contains("WITH RECURSIVE broken(hash) AS ("), "{sql}");
        assert!(
            sql.contains("FROM broken c, LATERAL ( SELECT r.referrer AS next"),
            "the recursive term walks referrer-ward: {sql}"
        );
        assert!(
            sql.contains("AND r.referrer <> c.hash OFFSET 0) s)"),
            "the recursive term must stay fenced and skip self-references: {sql}"
        );
    }

    /// The two seed arms are the whole definition of "not whole" at the leaves:
    /// a row with no NAR, and a reference to a hash `cached_path` does not have.
    /// Dropping the second reads an unknown path as present and the counter seeds
    /// low, which the first consistency sweep reports as drift.
    #[test]
    fn the_walk_seeds_from_the_unbacked_and_the_absent() {
        let sql = seed().split_whitespace().collect::<Vec<_>>().join(" ");

        assert!(
            sql.contains("SELECT cp.hash FROM cached_path cp WHERE cp.file_hash IS NULL"),
            "{sql}"
        );
        assert!(
            sql.contains(
                "LEFT JOIN cached_path dep ON dep.hash = r.reference_hash WHERE dep.hash IS NULL"
            ),
            "{sql}"
        );
    }

    /// The counter counts edges into the broken set, and a self-reference is not
    /// one of them: `gradient_db`'s live recount excludes `reference_hash = hash`,
    /// and a seed that counted it would disagree with every recompute.
    #[test]
    fn the_count_excludes_self_references() {
        let sql = seed().split_whitespace().collect::<Vec<_>>().join(" ");

        assert!(
            sql.contains(
                "JOIN broken b ON b.hash = r.reference_hash \
                 WHERE r.referrer <> r.reference_hash GROUP BY r.referrer"
            ),
            "{sql}"
        );
    }

    /// A row with nothing broken keeps the column default, so the seed touches
    /// only the rows it has a count for. The reset ahead of it is what clears a
    /// value a previous run left behind; without it a re-run leaves a stale
    /// non-zero on a row that has since become whole.
    #[test]
    fn the_seed_is_preceded_by_a_reset() {
        let stmts = up_statements();
        let reset = stmts
            .iter()
            .position(|s| {
                s == "UPDATE cached_path SET missing_references = 0 WHERE missing_references <> 0"
            })
            .expect("the reset runs");
        let seeded = stmts
            .iter()
            .position(|s| s.contains("WITH RECURSIVE broken"))
            .expect("the seed runs");

        assert!(reset < seeded, "the reset must precede the seed");
        assert!(
            stmts[0].contains("ADD COLUMN IF NOT EXISTS missing_references"),
            "the column exists before either: {:?}",
            stmts[0]
        );
    }
}
