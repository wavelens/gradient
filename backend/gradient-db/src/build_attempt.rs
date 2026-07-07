/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Attempt helpers, keyed on the global `derivation_build` anchor (the actual
//! build work). Each attempt is attributed to one `build_job` (the eval that
//! drove the dispatch, `None` after that eval is GC'd) and owns its log under
//! its own id.

use chrono::NaiveDateTime;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome, Column, Entity, Model};
use gradient_entity::ids::{
    BuildAttemptId, BuildJobId, DerivationBuildId, DispatchedJobId, EvaluationId,
};
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, DbErr, EntityTrait,
    IntoActiveModel, PaginatorTrait, QueryFilter, QueryOrder, Statement,
};
use uuid::Uuid;

/// Open a new attempt for an anchor (`derivation_build`), attributed to
/// `build_job`, under `dispatched_job`.
pub async fn open_attempt<C: ConnectionTrait>(
    db: &C,
    build_job: BuildJobId,
    derivation_build: DerivationBuildId,
    dispatched_job: DispatchedJobId,
    substitute: bool,
    build_context: serde_json::Value,
) -> Result<Model, DbErr> {
    Model {
        id: BuildAttemptId::now_v7(),
        build_job: Some(build_job),
        derivation_build,
        dispatched_job,
        substitute,
        outcome: AttemptOutcome::Running,
        build_context,
        created_at: gradient_types::now(),
        ..Default::default()
    }
    .into_active_model()
    .insert(db)
    .await
}

/// Count `SubstituteUnavailable` attempts per `(anchor, evaluation)`, for the
/// given anchor ids. The miss budget is scoped to the driving evaluation (via
/// the attempt's `build_job`) rather than the anchor's whole history, so a new
/// evaluation retries substitution from zero instead of inheriting a previous
/// eval's exhausted budget and escalating straight to a build. Pairs with zero
/// misses are absent from the map. Attempts orphaned by a GC'd eval
/// (`build_job IS NULL`) drop out of the inner join, as intended.
pub async fn substitute_miss_counts<C: ConnectionTrait>(
    db: &C,
    anchors: &[DerivationBuildId],
) -> Result<std::collections::HashMap<(DerivationBuildId, EvaluationId), i64>, DbErr> {
    let mut counts: std::collections::HashMap<(DerivationBuildId, EvaluationId), i64> =
        std::collections::HashMap::new();
    if anchors.is_empty() {
        return Ok(counts);
    }

    let rows = crate::fetch_in_chunks(anchors, |chunk| {
        let ids: Vec<Uuid> = chunk.iter().map(|a| a.into_inner()).collect();
        async move {
            db.query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                r#"SELECT ba.derivation_build AS anchor, bj.evaluation AS evaluation,
                          count(*) AS misses
                   FROM build_attempt ba
                   JOIN build_job bj ON bj.id = ba.build_job
                   WHERE ba.derivation_build = ANY($1) AND ba.reason = $2
                   GROUP BY ba.derivation_build, bj.evaluation"#,
                [
                    ids.into(),
                    (AttemptFailureReason::SubstituteUnavailable as i32).into(),
                ],
            ))
            .await
        }
    })
    .await?;

    for r in rows {
        let anchor = DerivationBuildId::new(r.try_get::<Uuid>("", "anchor")?);
        let evaluation = EvaluationId::new(r.try_get::<Uuid>("", "evaluation")?);
        let misses = r.try_get::<i64>("", "misses")?;
        counts.insert((anchor, evaluation), misses);
    }

    Ok(counts)
}

/// Most recent attempt for an anchor (by created_at desc), if any.
pub async fn latest_attempt<C: ConnectionTrait>(
    db: &C,
    derivation_build: DerivationBuildId,
) -> Result<Option<Model>, DbErr> {
    Entity::find()
        .filter(Column::DerivationBuild.eq(derivation_build))
        .order_by_desc(Column::CreatedAt)
        .one(db)
        .await
}

/// Most recent attempt for each anchor, fetched in one `DISTINCT ON` query per
/// chunk. Replaces per-anchor [`latest_attempt`] loops.
pub async fn latest_attempts<C: ConnectionTrait>(
    db: &C,
    anchors: &[DerivationBuildId],
) -> Result<std::collections::HashMap<DerivationBuildId, Model>, DbErr> {
    use sea_orm::{DbBackend, Statement};

    let rows = crate::fetch_in_chunks(anchors, |chunk| async move {
        let in_list = chunk
            .iter()
            .map(|id| format!("'{}'", id.into_inner()))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT DISTINCT ON (derivation_build) * FROM build_attempt \
             WHERE derivation_build IN ({in_list}) ORDER BY derivation_build, created_at DESC"
        );
        Entity::find()
            .from_raw_sql(Statement::from_string(DbBackend::Postgres, sql))
            .all(db)
            .await
    })
    .await?;

    Ok(rows.into_iter().map(|a| (a.derivation_build, a)).collect())
}

/// The log key to read/finalize for an anchor: its latest attempt's id. Returns
/// `None` when the anchor never produced an attempt (never dispatched).
pub async fn latest_attempt_id<C: ConnectionTrait>(
    db: &C,
    derivation_build: DerivationBuildId,
) -> Result<Option<BuildAttemptId>, DbErr> {
    Ok(latest_attempt(db, derivation_build).await?.map(|a| a.id))
}

/// The worker that ran the anchor's latest attempt (via its dispatched_job).
pub async fn latest_attempt_worker<C: ConnectionTrait>(
    db: &C,
    derivation_build: DerivationBuildId,
) -> Result<Option<String>, DbErr> {
    let Some(att) = latest_attempt(db, derivation_build).await? else {
        return Ok(None);
    };

    let job = gradient_entity::dispatched_job::Entity::find_by_id(att.dispatched_job)
        .one(db)
        .await?;

    Ok(job.map(|j| j.worker_id))
}

/// Stamp `build_started_at` on the latest attempt when its anchor enters Building.
pub async fn stamp_attempt_started<C: ConnectionTrait>(
    db: &C,
    derivation_build: DerivationBuildId,
    now: NaiveDateTime,
) -> Result<(), DbErr> {
    if let Some(att) = latest_attempt(db, derivation_build).await?
        && att.build_started_at.is_none()
    {
        let mut a = att.into_active_model();
        a.build_started_at = Set(Some(now));
        a.update(db).await?;
    }

    Ok(())
}

/// Record a terminal failure on the anchor's latest attempt: set `outcome` +
/// `reason` + `failure_message`, stamping `build_finished_at` if not already set.
pub async fn fail_latest_attempt<C: ConnectionTrait>(
    db: &C,
    derivation_build: DerivationBuildId,
    outcome: AttemptOutcome,
    reason: Option<AttemptFailureReason>,
    failure_message: Option<String>,
) -> Result<(), DbErr> {
    if let Some(att) = latest_attempt(db, derivation_build).await? {
        let mut a = att.clone().into_active_model();
        a.outcome = Set(outcome);
        a.reason = Set(reason);
        a.failure_message = Set(failure_message);
        if att.build_finished_at.is_none() {
            a.build_finished_at = Set(Some(gradient_types::now()));
        }

        a.update(db).await?;
    }

    Ok(())
}

/// Count `InputsUnavailable` attempts recorded against an anchor across its whole
/// history (every driving evaluation). Feeds the self-heal circuit breaker: each
/// failed eval reconciles the cache and the next one retries, so the count is the
/// number of self-heal loops already spent on this build.
pub async fn inputs_unavailable_attempt_count<C: ConnectionTrait>(
    db: &C,
    derivation_build: DerivationBuildId,
) -> Result<i64, DbErr> {
    Entity::find()
        .filter(Column::DerivationBuild.eq(derivation_build))
        .filter(Column::Reason.eq(AttemptFailureReason::InputsUnavailable))
        .count(db)
        .await
        .map(|c| c as i64)
}

/// Stamp `build_finished_at` on the latest attempt for any terminal status incl. Substituted.
pub async fn stamp_attempt_finished<C: ConnectionTrait>(
    db: &C,
    derivation_build: DerivationBuildId,
    now: NaiveDateTime,
) -> Result<(), DbErr> {
    if let Some(att) = latest_attempt(db, derivation_build).await?
        && att.build_finished_at.is_none()
    {
        let mut a = att.into_active_model();
        a.build_finished_at = Set(Some(now));
        a.update(db).await?;
    }

    Ok(())
}
