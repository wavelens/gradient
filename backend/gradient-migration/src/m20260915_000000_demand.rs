/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Demand-driven substitution (#593). No schema change beyond two indexes: this
//! renormalises every value the new predicates changed the meaning of.
//!
//! `fetchable` loses its `substitutable` arm, so "a dependent can get these
//! outputs" now means our own cache holds them whole. An anchor that was relayed
//! output-only therefore stops being ready for its dependents, and the first
//! statement resets those anchors so a demanded relay can make them whole; the
//! relay itself happens only when something asks for it, so the reset implies no
//! immediate work. `substituted` is cleared with the status for the same reason
//! `retire_paths` clears it: a `Created` row did not substitute anything.
//!
//! The gate gains a demand arm, which reads a DEPENDENT's status. That is safe
//! here only because the demote and the promote below both move rows inside
//! `(0, 1, 2, 8)`, so neither changes what the other's gate sees; the live
//! predicate carries the same argument.
//!
//! Every predicate is a frozen copy, deliberately unverified against
//! `gradient_db::graph_sql`: an equality test would compile and would then fail
//! the day the live predicate legitimately evolves, pressuring someone into
//! editing a shipped migration. The agreement is verified once, by review, at the
//! commit that introduces both.
//!
//! Order is load-bearing. The reset writes `status`, which `fetchable` reads;
//! `fetchable` is rewritten everywhere before `unready_deps` is counted from it,
//! because a counter computed from a stale `fetchable = true` is too LOW and too
//! low promotes.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const WHOLE: &str = "(cp.file_hash IS NOT NULL AND cp.missing_references = 0)";

const OUTPUT_NOT_WHOLE: &str = "EXISTS (SELECT 1 FROM derivation_output o \
     LEFT JOIN cached_path cp ON cp.hash = o.hash \
     WHERE o.derivation = db.derivation \
       AND NOT (cp.file_hash IS NOT NULL AND cp.missing_references = 0))";

fn fetchable(alias: &str) -> String {
    format!(
        "({alias}.status IN (3, 7) \
          AND EXISTS (SELECT 1 FROM derivation_output o2 WHERE o2.derivation = {alias}.derivation) \
          AND NOT EXISTS ( \
              SELECT 1 FROM derivation_output o LEFT JOIN cached_path cp ON cp.hash = o.hash \
              WHERE o.derivation = {alias}.derivation AND NOT {WHOLE}))"
    )
}

fn demanded() -> String {
    "(EXISTS (SELECT 1 FROM entry_point ep WHERE ep.derivation = db.derivation) \
      OR EXISTS (SELECT 1 FROM derivation_dependency e \
                 JOIN derivation_build p ON p.derivation = e.derivation \
                 JOIN derivation w ON w.id = p.derivation \
                 WHERE e.dependency = db.derivation AND w.walked AND NOT p.substitutable \
                   AND p.status IN (0, 1, 2, 8) \
                   AND EXISTS (SELECT 1 FROM build_job bj WHERE bj.derivation = p.derivation)))"
        .to_owned()
}

fn gates() -> String {
    format!(
        "(EXISTS (SELECT 1 FROM derivation w WHERE w.id = db.derivation AND w.walked) \
          AND EXISTS (SELECT 1 FROM build_job bj WHERE bj.derivation = db.derivation) \
          AND ((db.substitutable AND {demanded}) \
               OR (NOT db.substitutable AND db.unready_deps = 0 AND EXISTS ( \
                   SELECT 1 FROM derivation d JOIN cached_path cp ON cp.hash = d.hash \
                   WHERE d.id = db.derivation AND {WHOLE}))))",
        demanded = demanded(),
    )
}

/// `PROMOTE_ANY`'s partial-index bound. The gate implies it (a relay takes the
/// `substitutable` arm, a build the `unready_deps = 0` one), and
/// `gradient_db::readiness` repeats it verbatim as an explicit conjunct so
/// Postgres can prove the implication syntactically rather than having to
/// reason through the disjunction.
const PROMOTABLE_INDEX: &str = "CREATE INDEX IF NOT EXISTS \"idx-derivation_build-promotable\" \
     ON derivation_build (derivation) \
     WHERE status = 0 AND (unready_deps = 0 OR substitutable)";

fn up_statements() -> Vec<String> {
    vec![
        // The demand arm probes `entry_point` by derivation once per gated row.
        "CREATE INDEX IF NOT EXISTS \"idx-entry_point-derivation\" ON entry_point (derivation)"
            .into(),
        "DROP INDEX IF EXISTS \"idx-derivation_build-promotable\"".into(),
        PROMOTABLE_INDEX.into(),
        format!(
            "UPDATE derivation_build db SET status = 0, substituted = false, attempt = 0, \
                 updated_at = (now() AT TIME ZONE 'UTC') \
             WHERE db.substitutable AND db.status IN (3, 7) AND {OUTPUT_NOT_WHOLE}"
        ),
        format!(
            "UPDATE derivation_build db SET fetchable = f.v \
             FROM (SELECT b.id, {} AS v FROM derivation_build b) f \
             WHERE f.id = db.id AND db.fetchable <> f.v",
            fetchable("b"),
        ),
        "UPDATE derivation_build db SET unready_deps = coalesce(c.n, 0) \
         FROM derivation_build b \
         LEFT JOIN (SELECT e.derivation, count(*) AS n FROM derivation_dependency e \
                    LEFT JOIN derivation_build dep ON dep.derivation = e.dependency \
                    WHERE dep.derivation IS NULL OR NOT dep.fetchable \
                    GROUP BY e.derivation) c ON c.derivation = b.derivation \
         WHERE b.id = db.id AND db.unready_deps <> coalesce(c.n, 0)"
            .into(),
        format!(
            "UPDATE derivation_build db SET status = 0, updated_at = (now() AT TIME ZONE 'UTC') \
             WHERE db.status = 1 AND NOT {}",
            gates(),
        ),
        format!(
            "UPDATE derivation_build db SET status = 1, \
                 queued_at = coalesce(db.queued_at, now() AT TIME ZONE 'UTC'), \
                 updated_at = (now() AT TIME ZONE 'UTC') \
             WHERE db.status = 0 AND {}",
            gates(),
        ),
    ]
}

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

    /// Nothing to restore: no schema is changed that matters, and the old flag
    /// semantics ("an upstream copy is fetchable") cannot be recovered from the
    /// rows this leaves behind.
    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{PROMOTABLE_INDEX, fetchable, gates, up_statements};

    /// The whole point of the migration: an upstream copy stops counting, and the
    /// output guard that keeps an anchor with no output rows out of `fetchable`
    /// stays (the unbacked-output dead zone this project has already paid for).
    #[test]
    fn the_frozen_fetchable_drops_the_upstream_arm_and_keeps_the_output_guard() {
        let f = fetchable("b");
        assert!(!f.contains("substitutable"), "{f}");
        assert!(
            f.contains(
                "EXISTS (SELECT 1 FROM derivation_output o2 WHERE o2.derivation = b.derivation)"
            ),
            "{f}"
        );
    }

    /// Both normalising moves stay inside the status set the demand arm reads, so
    /// the demote cannot change what the promote's gate sees for another row.
    #[test]
    fn the_demote_and_the_promote_cannot_disturb_each_others_demand() {
        assert!(gates().contains("p.status IN (0, 1, 2, 8)"), "{}", gates());
        let stmts = up_statements();
        assert!(
            stmts
                .iter()
                .any(|s| s.contains("SET status = 0, updated_at"))
        );
        assert!(stmts.iter().any(|s| s.contains("SET status = 1,")));
    }

    /// `fetchable` must be rewritten everywhere before a counter is derived from
    /// it: a count over a stale `fetchable = true` is too low, and too low
    /// promotes an anchor onto an input nothing can provide.
    #[test]
    fn fetchable_is_rewritten_before_unready_deps_is_counted_from_it() {
        let stmts = up_statements();
        let f = stmts
            .iter()
            .position(|s| s.contains("SET fetchable = f.v"))
            .expect("the fetchable pass runs");
        let n = stmts
            .iter()
            .position(|s| s.contains("SET unready_deps = coalesce(c.n, 0)"))
            .expect("the counter pass runs");
        assert!(f < n, "{f} must precede {n}");
    }

    /// The counter pass is a recompute, not a seed: it must be able to bring a
    /// value DOWN to zero, which the grouped aggregate alone cannot do because a
    /// row with no unready edge produces no group.
    #[test]
    fn the_counter_pass_can_bring_a_row_back_to_zero() {
        let n = up_statements()
            .into_iter()
            .find(|s| s.contains("SET unready_deps = coalesce(c.n, 0)"))
            .expect("the counter pass runs");
        assert!(n.contains("LEFT JOIN (SELECT e.derivation"), "{n}");
        assert!(n.contains("db.unready_deps <> coalesce(c.n, 0)"), "{n}");
    }

    /// A substitutable anchor is now promotable with unready dependencies, so the
    /// partial index the table-wide promote matches has to admit it or that
    /// statement degrades to a sequential scan.
    #[test]
    fn the_promotable_index_admits_a_relay_with_unready_dependencies() {
        assert!(
            PROMOTABLE_INDEX.contains("WHERE status = 0 AND (unready_deps = 0 OR substitutable)"),
            "{PROMOTABLE_INDEX}"
        );
        assert!(up_statements().iter().any(|s| s == PROMOTABLE_INDEX));
    }
}
