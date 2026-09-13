/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Evaluation <-> derivation reachability over the global graph. A `build_job`
//! row exists for every derivation an evaluation needs, so it doubles as the
//! reachability link and the GC refcount: a derivation with no `build_job` is
//! reachable from no surviving evaluation.

use gradient_entity::build::BuildStatus;
use gradient_types::*;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, DbErr, EntityTrait, QueryFilter, QuerySelect,
    Statement,
};

/// Build status of every anchor an evaluation needs (one per `build_job`).
/// Used for graph-derived eval-done.
pub async fn eval_anchor_statuses<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<Vec<BuildStatus>, DbErr> {
    let anchor_ids: Vec<DerivationBuildId> = EBuildJob::find()
        .select_only()
        .column(CBuildJob::DerivationBuild)
        .filter(CBuildJob::Evaluation.eq(evaluation))
        .into_tuple::<DerivationBuildId>()
        .all(db)
        .await?;
    if anchor_ids.is_empty() {
        return Ok(vec![]);
    }

    let raw = crate::fetch_in_chunks(&anchor_ids, |chunk| async move {
        EDerivationBuild::find()
            .select_only()
            .column(CDerivationBuild::Status)
            .filter(CDerivationBuild::Id.is_in(chunk))
            .into_tuple::<i32>()
            .all(db)
            .await
    })
    .await?;

    Ok(raw
        .into_iter()
        .filter_map(|s| BuildStatus::try_from(s).ok())
        .collect())
}

/// The anchor's current status, for the dispatcher's last look before a
/// hand-out: a queued job whose gate regressed since it was enqueued reads
/// `Created` here and is dropped instead of dispatched with a missing input.
pub async fn anchor_status<C: ConnectionTrait>(
    db: &C,
    anchor: DerivationBuildId,
) -> Result<Option<BuildStatus>, DbErr> {
    Ok(EDerivationBuild::find_by_id(anchor)
        .one(db)
        .await?
        .map(|a| a.status))
}

/// Evaluations that reference `derivation` (via a `build_job`). Drives status
/// fan-out: a single anchor transition updates every referencing eval's view.
pub async fn evals_referencing_derivation<C: ConnectionTrait>(
    db: &C,
    derivation: DerivationId,
) -> Result<Vec<EvaluationId>, DbErr> {
    EBuildJob::find()
        .select_only()
        .column(CBuildJob::Evaluation)
        .distinct()
        .filter(CBuildJob::Derivation.eq(derivation))
        .into_tuple::<EvaluationId>()
        .all(db)
        .await
}

/// All `build_job` rows for `derivation`, across every evaluation that needs it.
pub async fn build_jobs_for_derivation<C: ConnectionTrait>(
    db: &C,
    derivation: DerivationId,
) -> Result<Vec<MBuildJob>, DbErr> {
    EBuildJob::find()
        .filter(CBuildJob::Derivation.eq(derivation))
        .all(db)
        .await
}

/// Bulk variant of [`build_jobs_for_derivation`]: one IN-list query for the
/// whole batch instead of a round-trip per derivation.
pub async fn build_jobs_for_derivations<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<std::collections::HashMap<DerivationId, Vec<MBuildJob>>, DbErr> {
    Ok(crate::fetch_in_chunks(derivations, |chunk| async move {
        EBuildJob::find()
            .filter(CBuildJob::Derivation.is_in(chunk))
            .all(db)
            .await
    })
    .await?
    .into_iter()
    .fold(std::collections::HashMap::new(), |mut m, j| {
        m.entry(j.derivation).or_default().push(j);
        m
    }))
}

/// The derivations whose outputs carry any of `hashes`: the anchors a store
/// path's arrival or removal can make fetchable or unfetchable.
pub async fn producers_of_hashes<C: ConnectionTrait>(
    db: &C,
    hashes: &[String],
) -> Result<Vec<DerivationId>, DbErr> {
    if hashes.is_empty() {
        return Ok(Vec::new());
    }

    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT DISTINCT o.derivation FROM derivation_output o WHERE o.hash = ANY($1)",
            [hashes.to_vec().into()],
        ))
        .await?;

    Ok(rows
        .iter()
        .filter_map(|r| r.try_get::<uuid::Uuid>("", "derivation").ok())
        .map(DerivationId::new)
        .collect())
}

/// The derivations whose own `.drv` hash is any of `hashes`: the anchors whose
/// own derivation file just arrived in, or left, the cache.
pub async fn derivations_with_hashes<C: ConnectionTrait>(
    db: &C,
    hashes: &[String],
) -> Result<Vec<DerivationId>, DbErr> {
    if hashes.is_empty() {
        return Ok(Vec::new());
    }

    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT d.id FROM derivation d WHERE d.hash = ANY($1)",
            [hashes.to_vec().into()],
        ))
        .await?;

    Ok(rows
        .iter()
        .filter_map(|r| r.try_get::<uuid::Uuid>("", "id").ok())
        .map(DerivationId::new)
        .collect())
}

/// Whether any surviving evaluation needs `derivation` (a `build_job` exists).
/// The refcount source for derivation GC.
pub async fn derivation_is_reachable<C: ConnectionTrait>(
    db: &C,
    derivation: DerivationId,
) -> Result<bool, DbErr> {
    Ok(EBuildJob::find()
        .filter(CBuildJob::Derivation.eq(derivation))
        .limit(1)
        .one(db)
        .await?
        .is_some())
}
