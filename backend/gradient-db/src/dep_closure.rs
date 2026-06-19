/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Incremental dependency-closure counts powering the project page (#383).
//!
//! A derivation's build-time closure is content-addressed, so it is
//! materialised once into `derivation_closure` and reused across evaluations;
//! its size is cached on `derivation.dep_closure_count`. The per-entry-point
//! build-status histogram lives in `entry_point_dep_count`, initialised once per
//! evaluation and then maintained incrementally on every build status
//! transition - replacing the per-request recursive closure walk in
//! [`crate::entry_point_dep_counts`].

use crate::closure::transitive_closure_reachable;
use crate::fetch_in_chunks;
use gradient_entity::build::BuildStatus;
use gradient_entity::ids::{DerivationClosureId, DerivationId, EntryPointId, EvaluationId};
use gradient_types::*;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbBackend, DbErr, EntityTrait, IntoActiveModel, QueryFilter,
    QuerySelect, Statement,
};
use std::collections::HashMap;

const INSERT_CHUNK: usize = 1000;

/// Materialise the build-time closure of every entry-point root derivation in
/// `evaluation` that is not already cached, and cache its size on
/// `derivation.dep_closure_count`. Idempotent: cached roots are skipped and edge
/// inserts conflict-do-nothing.
pub async fn materialize_entry_point_closures<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<(), DbErr> {
    let entry_points = EEntryPoint::find()
        .filter(CEntryPoint::Evaluation.eq(evaluation))
        .all(db)
        .await?;
    if entry_points.is_empty() {
        return Ok(());
    }

    let root_ids: Vec<DerivationId> = entry_points.iter().map(|ep| ep.derivation).collect();

    let roots = fetch_in_chunks(&root_ids, |chunk| async move {
        EDerivation::find().filter(CDerivation::Id.is_in(chunk)).all(db).await
    })
    .await?;

    for root in roots {
        if root.dep_closure_count.is_some() {
            continue;
        }

        let closure = transitive_closure_reachable(db, &[root.id]).await?;
        let edges: Vec<_> = closure
            .iter()
            .filter(|&&dep| dep != root.id)
            .map(|&dep| {
                MDerivationClosure {
                    id: DerivationClosureId::now_v7(),
                    root_derivation: root.id,
                    dep_derivation: dep,
                }
                .into_active_model()
            })
            .collect();

        for chunk in edges.chunks(INSERT_CHUNK) {
            EDerivationClosure::insert_many(chunk.to_vec())
                .on_conflict(
                    OnConflict::columns([
                        CDerivationClosure::RootDerivation,
                        CDerivationClosure::DepDerivation,
                    ])
                    .do_nothing()
                    .to_owned(),
                )
                .do_nothing()
                .exec(db)
                .await?;
        }

        let count = edges.len() as i64;
        db.execute(Statement::from_string(
            DbBackend::Postgres,
            format!(
                "UPDATE derivation SET dep_closure_count = {count} WHERE id = '{}'",
                root.id.into_inner()
            ),
        ))
        .await?;
    }

    Ok(())
}

/// Recompute the authoritative `entry_point_dep_count` histogram for every entry
/// point in `evaluation` from the materialised closure joined to this
/// evaluation's builds. Deletes the eval's existing rows first so a status that
/// dropped to zero leaves no stale row, then re-inserts. Idempotent; safe to run
/// as the initial seed (before the first transition) or as a reconcile.
pub async fn init_entry_point_dep_counts<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<(), DbErr> {
    let eval = evaluation.into_inner();
    db.execute(Statement::from_string(
        DbBackend::Postgres,
        format!(
            "DELETE FROM entry_point_dep_count \
             WHERE entry_point IN (SELECT id FROM entry_point WHERE evaluation = '{eval}')"
        ),
    ))
    .await?;
    db.execute(Statement::from_string(
        DbBackend::Postgres,
        format!(
            "INSERT INTO entry_point_dep_count (id, entry_point, status, count) \
             SELECT uuidv7(), ep.id, b.status, COUNT(*) \
             FROM entry_point ep \
             JOIN derivation_closure dc ON dc.root_derivation = ep.derivation \
             JOIN derivation_build b ON b.derivation = dc.dep_derivation \
             WHERE ep.evaluation = '{eval}' \
             GROUP BY ep.id, b.status"
        ),
    ))
    .await?;
    Ok(())
}

/// Ensure `evaluation`'s closures are materialised and recompute its maintained
/// counts from scratch. Used to re-sync after bulk status writes that bypass
/// [`apply_dep_count_delta`] (restart recovery, abort-on-disconnect).
pub async fn reconcile_eval_dep_counts<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<(), DbErr> {
    materialize_entry_point_closures(db, evaluation).await?;
    init_entry_point_dep_counts(db, evaluation).await
}

/// Seed an evaluation's maintained counts once its build graph is complete.
/// Returns early (single cheap query) when the evaluation has no entry points,
/// otherwise materialises closures and recomputes the histogram. Must run before
/// the first build status transition so subsequent deltas have a baseline.
pub async fn seed_entry_point_dep_counts<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<(), DbErr> {
    let any = EEntryPoint::find()
        .filter(CEntryPoint::Evaluation.eq(evaluation))
        .limit(1)
        .all(db)
        .await?;
    if any.is_empty() {
        return Ok(());
    }

    reconcile_eval_dep_counts(db, evaluation).await
}

/// Re-sync maintained counts for every in-flight evaluation. Called once after
/// startup recovery, where bulk re-queue / abort changes build statuses outside
/// the per-transition hook.
pub async fn reconcile_inflight_dep_counts<C: ConnectionTrait>(db: &C) -> Result<(), DbErr> {
    use gradient_entity::evaluation::EvaluationStatus;

    let evals = EEvaluation::find()
        .filter(CEvaluation::Status.is_in([EvaluationStatus::Building, EvaluationStatus::Waiting]))
        .all(db)
        .await?;
    for eval in evals {
        reconcile_eval_dep_counts(db, eval.id).await?;
    }

    Ok(())
}

/// Move one unit from `old_status` to `new_status` for every entry point (in
/// any evaluation) whose closure contains `dep_derivation`. The anchor is
/// global, so a single transition fans across all referencing entry points.
/// Called from `update_derivation_build_status` on each transition; a no-op for
/// entry points without a materialised closure, so it is always safe to call.
pub async fn apply_dep_count_delta<C: ConnectionTrait>(
    db: &C,
    dep_derivation: DerivationId,
    old_status: i32,
    new_status: i32,
) -> Result<(), DbErr> {
    let dep = dep_derivation.into_inner();
    let affected = "FROM entry_point ep \
         JOIN derivation_closure dc ON dc.root_derivation = ep.derivation";
    let predicate = format!("dc.dep_derivation = '{dep}'");

    db.execute(Statement::from_string(
        DbBackend::Postgres,
        format!(
            "UPDATE entry_point_dep_count c SET count = c.count - 1 \
             {affected} \
             WHERE c.entry_point = ep.id AND c.status = {old_status} AND c.count > 0 \
               AND {predicate}"
        ),
    ))
    .await?;

    db.execute(Statement::from_string(
        DbBackend::Postgres,
        format!(
            "INSERT INTO entry_point_dep_count (id, entry_point, status, count) \
             SELECT uuidv7(), ep.id, {new_status}, 1 \
             {affected} \
             WHERE {predicate} \
             ON CONFLICT (entry_point, status) DO UPDATE SET count = entry_point_dep_count.count + 1"
        ),
    ))
    .await?;

    Ok(())
}

/// Load the maintained `entry_point -> status -> count` histogram. Returns an
/// empty map when no counts are maintained (caller falls back to the live CTE).
pub async fn load_entry_point_dep_counts<C: ConnectionTrait>(
    db: &C,
    entry_points: &[EntryPointId],
) -> Result<HashMap<EntryPointId, HashMap<BuildStatus, i64>>, DbErr> {
    let rows = fetch_in_chunks(entry_points, |chunk| async move {
        EEntryPointDepCount::find()
            .filter(CEntryPointDepCount::EntryPoint.is_in(chunk))
            .all(db)
            .await
    })
    .await?;

    let mut out: HashMap<EntryPointId, HashMap<BuildStatus, i64>> = HashMap::new();
    for r in rows {
        if let Ok(status) = BuildStatus::try_from(r.status) {
            *out.entry(r.entry_point).or_default().entry(status).or_insert(0) += r.count;
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::ids::EntryPointDepCountId;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn count_row(entry_point: EntryPointId, status: BuildStatus, count: i64) -> MEntryPointDepCount {
        gradient_entity::entry_point_dep_count::Model {
            id: EntryPointDepCountId::now_v7(),
            entry_point,
            status: i32::from(status),
            count,
        }
    }

    #[tokio::test]
    async fn load_groups_rows_by_entry_point_and_status() {
        let ep1 = EntryPointId::now_v7();
        let ep2 = EntryPointId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                count_row(ep1, BuildStatus::Completed, 3),
                count_row(ep1, BuildStatus::Building, 1),
                count_row(ep2, BuildStatus::Queued, 5),
            ]])
            .into_connection();

        let out = load_entry_point_dep_counts(&db, &[ep1, ep2]).await.unwrap();

        assert_eq!(out[&ep1][&BuildStatus::Completed], 3);
        assert_eq!(out[&ep1][&BuildStatus::Building], 1);
        assert_eq!(out[&ep2][&BuildStatus::Queued], 5);
    }

    #[tokio::test]
    async fn load_returns_empty_when_no_counts_maintained() {
        let ep = EntryPointId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MEntryPointDepCount>::new()])
            .into_connection();

        let out = load_entry_point_dep_counts(&db, &[ep]).await.unwrap();

        assert!(out.is_empty());
    }
}
