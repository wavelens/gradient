/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Every predicate here is a frozen copy of `gradient_db::graph_sql` and is deliberately unverified
//! against it. An equality test would fail after any change to the live predicate. Order is
//! load-bearing. `fetchable` must be rewritten everywhere before `unready_deps` is counted from it.
//! A count over a stale `fetchable = true` is too low and would promote too early.

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

const PROMOTABLE_INDEX: &str = "CREATE INDEX IF NOT EXISTS \"idx-derivation_build-promotable\" \
     ON derivation_build (derivation) \
     WHERE status = 0 AND (unready_deps = 0 OR substitutable)";

fn up_statements() -> Vec<String> {
    vec![
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

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{PROMOTABLE_INDEX, fetchable, gates, up_statements};

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

    #[test]
    fn the_counter_pass_can_bring_a_row_back_to_zero() {
        let n = up_statements()
            .into_iter()
            .find(|s| s.contains("SET unready_deps = coalesce(c.n, 0)"))
            .expect("the counter pass runs");
        assert!(n.contains("LEFT JOIN (SELECT e.derivation"), "{n}");
        assert!(n.contains("db.unready_deps <> coalesce(c.n, 0)"), "{n}");
    }

    #[test]
    fn the_promotable_index_admits_a_relay_with_unready_dependencies() {
        assert!(
            PROMOTABLE_INDEX.contains("WHERE status = 0 AND (unready_deps = 0 OR substitutable)"),
            "{PROMOTABLE_INDEX}"
        );
        assert!(up_statements().iter().any(|s| s == PROMOTABLE_INDEX));
    }
}
