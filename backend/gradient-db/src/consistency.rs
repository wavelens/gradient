/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Read-only build-graph invariant assertions. Counts violations of the
//! invariants the dispatch/promotion gates trust, so a dead zone surfaces as a
//! warning metric instead of a user-reported stuck evaluation. Reuses the very
//! gate SQL the reconciler maintains, so a non-zero count means "the healing
//! pipeline is not converging", never "the checker disagrees with the gates".
//! Transient non-zero counts between a transition and the next reconcile tick
//! are expected; persistent counts are the alert.

use sea_orm::{ConnectionTrait, DatabaseBackend, DbErr, Statement};

/// Counts of graph-invariant violations at one instant.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct ConsistencyReport {
    /// Anchors trusted `closure_complete` whose gate no longer holds.
    pub stale_closure_complete: i64,
    /// Anchors trusted `drv_closure_cached` whose gate no longer holds.
    pub stale_drv_closure_cached: i64,
    /// `Created` anchors that pass the full promotion predicate yet sit unpromoted.
    pub unpromoted_ready: i64,
    /// Outputs of terminal-success producers with no backing artifact.
    pub unbacked_trusted_outputs: i64,
    /// `Building` evaluations with zero non-terminal anchors left.
    pub wedged_building_evals: i64,
}

impl ConsistencyReport {
    pub fn total(&self) -> i64 {
        self.stale_closure_complete
            + self.stale_drv_closure_cached
            + self.unpromoted_ready
            + self.unbacked_trusted_outputs
            + self.wedged_building_evals
    }
}

async fn count<C: ConnectionTrait>(db: &C, sql: String) -> Result<i64, DbErr> {
    let row = db
        .query_one(Statement::from_string(DatabaseBackend::Postgres, sql))
        .await?;
    Ok(row.and_then(|r| r.try_get::<i64>("", "n").ok()).unwrap_or(0))
}

/// Count every invariant violation the gates could act on right now.
pub async fn graph_consistency_report<C: ConnectionTrait>(
    db: &C,
) -> Result<ConsistencyReport, DbErr> {
    let closure_gate = crate::promotion::CLOSURE_COMPLETE_GATE;
    let drv_gate = crate::promotion::DRV_CLOSURE_CACHED_GATE;
    let deps_ready = crate::graph_sql::deps_ready_predicate("db");
    let unbacked = crate::cache_storage::UNBACKED_TRUSTED_OUTPUTS_SELECT;

    let stale_closure_complete = count(
        db,
        format!(
            "SELECT count(*) AS n FROM derivation_build db \
             WHERE db.closure_complete AND NOT ({closure_gate})"
        ),
    )
    .await?;

    let stale_drv_closure_cached = count(
        db,
        format!(
            "SELECT count(*) AS n FROM derivation_build db \
             WHERE db.drv_closure_cached AND NOT ({drv_gate})"
        ),
    )
    .await?;

    let unpromoted_ready = count(
        db,
        format!(
            "SELECT count(*) AS n FROM derivation_build db \
             WHERE db.status = 0 \
               AND db.edges_complete \
               AND EXISTS (SELECT 1 FROM build_job bj WHERE bj.derivation = db.derivation) \
               AND (db.substitutable OR ({deps_ready}))"
        ),
    )
    .await?;

    let unbacked_trusted_outputs =
        count(db, format!("SELECT count(*) AS n FROM ({unbacked}) u")).await?;

    let building = gradient_entity::evaluation::EvaluationStatus::Building as i32;
    let wedged_building_evals = count(
        db,
        format!(
            "SELECT count(*) AS n FROM evaluation ev \
             WHERE ev.status = {building} \
               AND NOT EXISTS ( \
                 SELECT 1 FROM build_job bj \
                 JOIN derivation_build db ON db.derivation = bj.derivation \
                 WHERE bj.evaluation = ev.id AND db.status IN (0, 1, 2, 8))"
        ),
    )
    .await?;

    Ok(ConsistencyReport {
        stale_closure_complete,
        stale_drv_closure_cached,
        unpromoted_ready,
        unbacked_trusted_outputs,
        wedged_building_evals,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_sums_every_dimension() {
        let r = ConsistencyReport {
            stale_closure_complete: 1,
            stale_drv_closure_cached: 2,
            unpromoted_ready: 3,
            unbacked_trusted_outputs: 4,
            wedged_building_evals: 5,
        };
        assert_eq!(r.total(), 15);
        assert_eq!(ConsistencyReport::default().total(), 0);
    }
}
