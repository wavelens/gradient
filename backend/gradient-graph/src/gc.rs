/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The re-check after the pool scan is exact because reachability can only grow through
//! new roots. An evaluation walking a derivation is writing its own `build_job` rows in the
//! same batch. The closure of every root created since the scan is covering all it missed.

use std::collections::HashSet;

use anyhow::{Context, Result};
use chrono::NaiveDateTime;
use gradient_db::graph::walks::{ClosureDirection, dependency_closure_cte_body};
use gradient_db::{DbContext, graph::runtime_can_start::retire_outputs};
use gradient_types::ids::{BuildAttemptId, DerivationId, EvaluationId};
use gradient_types::*;
use ractor::ActorRef;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ConnectionTrait, EntityTrait, IntoActiveModel,
    TransactionTrait, Value,
};
use tracing::info;
use uuid::Uuid;

use crate::messages::{GcReport, GcRequest};
use crate::writer::GraphMsg;

fn fresh_cte() -> String {
    dependency_closure_cte_body(
        "fresh",
        "SELECT derivation FROM build_job WHERE created_at >= $2 \
         UNION SELECT derivation FROM entry_point WHERE created_at >= $2",
        ClosureDirection::Dependencies,
    )
}

fn delete_derivations_sql() -> String {
    format!(
        "WITH RECURSIVE {fresh} \
         DELETE FROM derivation d WHERE d.id = ANY($1) \
           AND NOT EXISTS (SELECT 1 FROM fresh f WHERE f.derivation = d.id) \
         RETURNING d.id",
        fresh = fresh_cte(),
    )
}

fn stale_after_scan_sql() -> String {
    format!(
        "WITH RECURSIVE {fresh}, \
         roots(derivation) AS ( \
             SELECT derivation FROM fresh \
             UNION SELECT id FROM derivation WHERE created_at >= $2 \
             UNION SELECT derivation FROM derivation_output WHERE created_at >= $2), \
         {runtime}, {live} \
         SELECT u.h AS hash FROM unnest($1::text[]) AS u(h) \
         WHERE NOT EXISTS (SELECT 1 FROM live l WHERE l.hash = u.h)",
        fresh = fresh_cte(),
        runtime = gradient_db::graph::walks::runtime_closure_cte_body(
            "runtime",
            "SELECT derivation FROM roots",
        ),
        live = gradient_db::graph::walks::kept_hashes_cte_body("roots", "runtime"),
    )
}

gradient_db::sql_fn! {
    GC_DELETE_DERIVATIONS = delete_derivations_sql,
        params = [OrphanDerivationIds(64), Now],
        tier = Walk,
        flags = [Walk];

    GC_STALE_AFTER_SCAN = stale_after_scan_sql,
        params = [CachedPathHashes(64), Now],
        tier = Sweep,
        flags = [Walk];
}

gradient_db::sql! {
    GC_CANDIDATE_ATTEMPTS = "SELECT b.derivation AS derivation, a.id AS attempt \
             FROM build_attempt a JOIN derivation_build b ON b.id = a.derivation_build \
             WHERE b.derivation = ANY($1)",
        params = [DerivationIds(64)],
        tier = Sweep;

    GC_CANDIDATE_PARENTS = "SELECT e.derivation, e.dependency FROM derivation_dependency e \
             WHERE e.dependency = ANY($1)",
        params = [DerivationIds(64)],
        tier = Sweep;

    GC_SHARED_BUILDS_LOSING_AN_EVALUATION = "WITH named AS MATERIALIZED ( \
                 SELECT DISTINCT derivation FROM build_job WHERE evaluation = ANY($1::uuid[])) \
             SELECT derivation FROM named \
             UNION SELECT derivation FROM entry_point WHERE evaluation = ANY($1::uuid[]) \
             UNION SELECT e.dependency AS derivation FROM derivation_dependency e \
             JOIN named n ON n.derivation = e.derivation",
        params = [EvaluationIds(64)],
        tier = Sweep;
}

pub(crate) async fn gc_task_evaluations(
    ctx: &DbContext,
    writer: ActorRef<GraphMsg>,
    task_id: TaskId,
    keep: usize,
) -> Result<()> {
    let plan = gradient_db::maintenance::gc::evaluation_gc_plan(ctx, task_id, keep).await?;

    for chunk in plan.chunks(gradient_db::IN_CHUNK_SIZE) {
        let ids: Vec<EvaluationId> = chunk.iter().map(|e| e.id).collect();
        let report = match writer
            .call(
                |reply| GraphMsg::Gc(GcRequest::Evaluations { ids }, reply),
                None,
            )
            .await
        {
            Ok(ractor::rpc::CallResult::Success(result)) => result?,
            other => anyhow::bail!("the graph writer did not apply the evaluation GC: {other:?}"),
        };

        let deleted: Vec<MEvaluation> = chunk
            .iter()
            .filter(|e| report.deleted_evaluations.contains(&e.id))
            .cloned()
            .collect();
        gradient_db::maintenance::gc::after_evaluation_delete(ctx, &deleted).await?;
    }

    Ok(())
}

pub(crate) async fn apply(ctx: &DbContext, req: GcRequest) -> Result<GcReport> {
    match req {
        GcRequest::Derivations {
            candidates,
            scanned_at,
        } => delete_derivations(ctx, &candidates, scanned_at).await,
        GcRequest::Paths { hashes, scanned_at } => {
            retire_stale_paths(ctx, &hashes, scanned_at).await
        }
        GcRequest::Evaluations { ids } => delete_evaluations(ctx, &ids).await,
    }
}

fn ids(derivations: &[DerivationId]) -> Value {
    derivations
        .iter()
        .map(|d| d.into_inner())
        .collect::<Vec<Uuid>>()
        .into()
}

async fn delete_derivations(
    ctx: &DbContext,
    candidates: &[DerivationId],
    scanned_at: NaiveDateTime,
) -> Result<GcReport> {
    let db = &ctx.worker_db;
    if candidates.is_empty() {
        return Ok(GcReport::default());
    }

    let attempts = pairs(db, GC_CANDIDATE_ATTEMPTS.bind([ids(candidates)]), "attempt")
        .await
        .context("GC: failed to snapshot the candidates' build attempts")?;
    let wanted_by = pairs(
        db,
        GC_CANDIDATE_PARENTS.bind([ids(candidates)]),
        "dependency",
    )
    .await
    .context("GC: failed to snapshot the edges into the candidates")?;

    let deleted: HashSet<DerivationId> = db
        .query_all_raw(
            GC_DELETE_DERIVATIONS.bind([ids(candidates), Value::ChronoDateTime(Some(scanned_at))]),
        )
        .await
        .context("GC: failed to delete the orphan derivations")?
        .iter()
        .filter_map(|r| r.try_get::<Uuid>("", "id").ok())
        .map(DerivationId::new)
        .collect();

    if deleted.is_empty() {
        return Ok(GcReport::default());
    }

    let survivors: Vec<DerivationId> = orphaned_survivors(&wanted_by, &deleted);
    if !survivors.is_empty() {
        let txn = db.begin().await.context("GC: begin the survivor un-walk")?;
        gradient_db::graph::walk_completeness::unwalk(&txn, &survivors)
            .await
            .context("GC: failed to re-walk the survivors of a deleted dependency")?;
        txn.commit()
            .await
            .context("GC: commit the survivor un-walk")?;
        let changes = gradient_db::graph::can_start::unpromote_ungated(db, &survivors)
            .await
            .context("GC: failed to settle the survivors of a deleted dependency")?;
        gradient_db::status::emit_transition_effects(ctx, &changes).await?;
    }

    info!(deleted = deleted.len(), "Orphan derivation GC done");

    Ok(GcReport {
        attempt_logs: attempts
            .into_iter()
            .filter(|(derivation, _)| deleted.contains(derivation))
            .map(|(_, attempt)| BuildAttemptId::new(attempt))
            .collect(),
        deleted_derivations: deleted.into_iter().collect(),
        ..Default::default()
    })
}

async fn retire_stale_paths(
    ctx: &DbContext,
    hashes: &[String],
    scanned_at: NaiveDateTime,
) -> Result<GcReport> {
    let db = &ctx.worker_db;
    if hashes.is_empty() {
        return Ok(GcReport::default());
    }

    let still_stale: Vec<String> = db
        .query_all_raw(GC_STALE_AFTER_SCAN.bind([
            hashes.to_vec().into(),
            Value::ChronoDateTime(Some(scanned_at)),
        ]))
        .await
        .context("GC: failed to re-check what became live since the scan")?
        .iter()
        .filter_map(|r| r.try_get::<String>("", "hash").ok())
        .collect();

    if still_stale.is_empty() {
        return Ok(GcReport::default());
    }

    // `retire_outputs` must still hold its opening locks when its DELETE is executing.
    // Inside the graph writer its transaction is a savepoint.
    // Postgres is keeping a subtransaction's locks until the outer commit.
    // The release below can therefore never drop one early.
    let savepoint = db
        .begin()
        .await
        .context("GC: failed to open the retire savepoint")?;
    let retired = retire_outputs(&savepoint, &still_stale)
        .await
        .context("GC: failed to retire the stale paths")?;
    savepoint
        .commit()
        .await
        .context("GC: failed to release the retire savepoint")?;
    gradient_db::status::emit_transition_effects(ctx, &retired.transitions).await?;

    Ok(GcReport {
        retired: retired.deleted,
        ..Default::default()
    })
}

async fn delete_evaluations(ctx: &DbContext, evaluations: &[EvaluationId]) -> Result<GcReport> {
    let db = &ctx.worker_db;
    if evaluations.is_empty() {
        return Ok(GcReport::default());
    }

    let raw: Vec<Uuid> = evaluations.iter().map(|e| e.into_inner()).collect();
    let lost: Vec<DerivationId> = db
        .query_all_raw(GC_SHARED_BUILDS_LOSING_AN_EVALUATION.bind([raw.into()]))
        .await
        .context("GC: failed to collect the derivations of the evaluations to delete")?
        .iter()
        .filter_map(|r| r.try_get::<Uuid>("", "derivation").ok())
        .map(DerivationId::new)
        .collect();

    unlink(db, evaluations)
        .await
        .context("GC: failed to NULL evaluation linked-list pointers")?;

    for id in evaluations {
        EEvaluation::delete_by_id(*id)
            .exec(db)
            .await
            .context("GC: failed to delete an evaluation")?;
    }

    let (adopted, changes) = gradient_db::maintenance::gc::settle_after_delete(ctx, &lost)
        .await
        .context("GC: failed to settle the queue after deleting evaluations")?;
    gradient_db::status::emit_transition_effects(ctx, &changes).await?;

    info!(
        deleted = evaluations.len(),
        adopted, "Per-task evaluation GC done"
    );

    Ok(GcReport {
        deleted_evaluations: evaluations.to_vec(),
        ..Default::default()
    })
}

async fn unlink<C: ConnectionTrait>(
    db: &C,
    deleted: &[EvaluationId],
) -> Result<(), sea_orm::DbErr> {
    use sea_orm::{ColumnTrait, Condition, QueryFilter};

    let gone: HashSet<EvaluationId> = deleted.iter().copied().collect();
    let touching = EEvaluation::find()
        .filter(
            Condition::any()
                .add(CEvaluation::Id.is_in(deleted.to_vec()))
                .add(CEvaluation::Previous.is_in(deleted.to_vec()))
                .add(CEvaluation::Next.is_in(deleted.to_vec())),
        )
        .all(db)
        .await?;

    for eval in touching {
        let drop_prev = eval.previous.is_some_and(|p| gone.contains(&p));
        let drop_next = eval.next.is_some_and(|n| gone.contains(&n));
        let mut a: AEvaluation = eval.clone().into_active_model();
        if gone.contains(&eval.id) || drop_prev {
            a.previous = Set(None);
        }
        if gone.contains(&eval.id) || drop_next {
            a.next = Set(None);
        }
        a.update(db).await?;
    }

    Ok(())
}

async fn pairs<C: ConnectionTrait>(
    db: &C,
    stmt: sea_orm::Statement,
    value_column: &str,
) -> Result<Vec<(DerivationId, Uuid)>, sea_orm::DbErr> {
    Ok(db
        .query_all_raw(stmt)
        .await?
        .iter()
        .filter_map(|r| {
            Some((
                DerivationId::new(r.try_get::<Uuid>("", "derivation").ok()?),
                r.try_get::<Uuid>("", value_column).ok()?,
            ))
        })
        .collect())
}

fn orphaned_survivors(
    wanted_by: &[(DerivationId, Uuid)],
    deleted: &HashSet<DerivationId>,
) -> Vec<DerivationId> {
    let mut survivors: Vec<DerivationId> = wanted_by
        .iter()
        .filter(|(parent, dependency)| {
            deleted.contains(&DerivationId::new(*dependency)) && !deleted.contains(parent)
        })
        .map(|(parent, _)| *parent)
        .collect();
    survivors.sort_unstable();
    survivors.dedup();

    survivors
}

#[cfg(test)]
fn attempt_logs_to_reclaim(
    snapshot: &[(DerivationId, Uuid)],
    deleted: &HashSet<DerivationId>,
) -> Vec<Uuid> {
    snapshot
        .iter()
        .filter(|(derivation, _)| deleted.contains(derivation))
        .map(|(_, attempt)| *attempt)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_ctx::ctx;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::collections::BTreeMap;

    fn norm(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn empty() -> Vec<BTreeMap<String, Value>> {
        Vec::new()
    }

    #[tokio::test]
    async fn the_delete_excludes_what_a_root_created_since_the_scan_reaches() {
        let d = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([empty(), empty(), empty()])
            .into_connection();
        let (ctx, pool) = ctx(db).await;

        let report = delete_derivations(&ctx, &[d], gradient_types::now())
            .await
            .expect("the delete runs");
        drop(ctx);

        assert!(report.deleted_derivations.is_empty());
        let log = gradient_db::pool::statements(pool.into_transaction_log());
        let delete = norm(
            log.iter()
                .find(|s| s.contains("DELETE FROM derivation d"))
                .expect("the delete runs"),
        );
        assert!(
            delete.contains(
                "fresh(derivation) AS (SELECT derivation FROM build_job WHERE created_at >= $2"
            ),
            "{delete}"
        );
        assert!(
            delete.contains("UNION SELECT derivation FROM entry_point WHERE created_at >= $2"),
            "{delete}"
        );
        assert!(
            delete.contains(
                "AND NOT EXISTS (SELECT 1 FROM fresh f WHERE f.derivation = d.id) RETURNING d.id"
            ),
            "{delete}"
        );
    }

    #[tokio::test]
    async fn the_retire_excludes_fresh_parents_and_the_fresh_closure() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([empty()])
            .into_connection();
        let (ctx, pool) = ctx(db).await;

        let report = retire_stale_paths(&ctx, &["abc".to_owned()], gradient_types::now())
            .await
            .expect("the re-check runs");
        drop(ctx);

        assert!(report.retired.is_empty());
        let log = gradient_db::pool::statements(pool.into_transaction_log());
        let sql = norm(&log[0]);
        assert!(sql.contains("e.kind IN (1, 2)"), "{sql}");
        assert!(
            sql.contains("UNION SELECT id FROM derivation WHERE created_at >= $2")
                && sql.contains(
                    "UNION SELECT derivation FROM derivation_output WHERE created_at >= $2"
                ),
            "{sql}"
        );
        assert!(sql.contains("live(hash) AS ("), "{sql}");
        assert!(!sql.contains("cached_path_reference"), "{sql}");
    }

    #[tokio::test]
    async fn an_empty_request_costs_no_statement() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let (ctx, pool) = ctx(db).await;

        assert!(
            apply(
                &ctx,
                GcRequest::Derivations {
                    candidates: Vec::new(),
                    scanned_at: gradient_types::now()
                }
            )
            .await
            .unwrap()
            .deleted_derivations
            .is_empty()
        );
        drop(ctx);

        assert!(gradient_db::pool::statements(pool.into_transaction_log()).is_empty());
    }

    #[test]
    fn only_a_survivor_of_a_reclaimed_dependency_is_re_walked() {
        let survivor = DerivationId::now_v7();
        let gone_parent = DerivationId::now_v7();
        let dependency = DerivationId::now_v7();
        let deleted: HashSet<DerivationId> = [dependency, gone_parent].into_iter().collect();

        assert_eq!(
            orphaned_survivors(
                &[
                    (survivor, dependency.into_inner()),
                    (gone_parent, dependency.into_inner()),
                ],
                &deleted,
            ),
            vec![survivor],
        );
    }

    #[test]
    fn only_a_reclaimed_derivations_logs_are_reported() {
        let gone = DerivationId::now_v7();
        let kept = DerivationId::now_v7();
        let gone_attempt = Uuid::now_v7();
        let deleted: HashSet<DerivationId> = [gone].into_iter().collect();

        assert_eq!(
            attempt_logs_to_reclaim(&[(gone, gone_attempt), (kept, Uuid::now_v7())], &deleted),
            vec![gone_attempt],
        );
    }
}
