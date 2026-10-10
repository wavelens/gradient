/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome, Column, Entity, Model};
use gradient_entity::ids::{
    BuildAttemptId, BuildJobId, DerivationBuildId, DerivationId, DispatchedJobId, EvaluationId,
};
use sea_orm::ActiveValue::Set;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    PaginatorTrait, QueryFilter, QueryOrder,
};
use uuid::Uuid;

crate::sql! {
    SUBSTITUTE_MISS_COUNTS = r#"SELECT ba.derivation_build AS shared_build, bj.evaluation AS evaluation,
                          count(*) AS misses
                   FROM build_attempt ba
                   JOIN build_job bj ON bj.id = ba.build_job
                   WHERE ba.derivation_build = ANY($1) AND ba.reason = $2
                   GROUP BY ba.derivation_build, bj.evaluation"#,
        params = [SharedBuildIds(64), Int(0)];

    WORKER_LOSS_STREAKS = r#"SELECT ba.derivation_build AS shared_build, count(*) AS losses
                   FROM build_attempt ba
                   WHERE ba.derivation_build = ANY($1) AND ba.outcome = $2 AND ba.reason = $3
                     AND NOT EXISTS (
                         SELECT 1 FROM build_attempt later
                         WHERE later.derivation_build = ba.derivation_build
                           AND later.created_at > ba.created_at
                           AND (later.outcome <> $2 OR later.reason IS DISTINCT FROM $3))
                   GROUP BY ba.derivation_build"#,
        params = [SharedBuildIds(64), Int(4), Int(9)];

    LATEST_ATTEMPT_EVALUATION = "SELECT bj.evaluation FROM build_attempt ba \
             JOIN build_job bj ON bj.id = ba.build_job \
             WHERE ba.derivation_build = $1 \
             ORDER BY ba.created_at DESC LIMIT 1",
        params = [SharedBuildId];
}

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

/// The miss budget is scoped to the driving evaluation, not the shared build's history.
/// A new evaluation is retrying substitution from zero instead of escalating to a build.
pub async fn substitute_miss_counts<C: ConnectionTrait>(
    db: &C,
    shared_builds: &[DerivationBuildId],
) -> Result<std::collections::HashMap<(DerivationBuildId, EvaluationId), i64>, DbErr> {
    let mut counts: std::collections::HashMap<(DerivationBuildId, EvaluationId), i64> =
        std::collections::HashMap::new();
    if shared_builds.is_empty() {
        return Ok(counts);
    }

    let rows = crate::fetch_in_chunks(shared_builds, |chunk| {
        let ids: Vec<Uuid> = chunk.iter().map(|a| a.into_inner()).collect();
        async move {
            db.query_all_raw(SUBSTITUTE_MISS_COUNTS.bind([
                ids.into(),
                (AttemptFailureReason::SubstituteUnavailable as i32).into(),
            ]))
            .await
        }
    })
    .await?;

    for r in rows {
        let shared_build = DerivationBuildId::new(r.try_get::<Uuid>("", "shared_build")?);
        let evaluation = EvaluationId::new(r.try_get::<Uuid>("", "evaluation")?);
        let misses = r.try_get::<i64>("", "misses")?;
        counts.insert((shared_build, evaluation), misses);
    }

    Ok(counts)
}

pub async fn latest_attempt_evaluation<C: ConnectionTrait>(
    db: &C,
    derivation_build: DerivationBuildId,
) -> Result<Option<EvaluationId>, DbErr> {
    let row = db
        .query_one_raw(LATEST_ATTEMPT_EVALUATION.bind([derivation_build.into_inner().into()]))
        .await?;

    row.map(|r| r.try_get::<Uuid>("", "evaluation").map(EvaluationId::new))
        .transpose()
}

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

fn latest_attempts_sql(chunk: &[DerivationBuildId]) -> String {
    let in_list = chunk
        .iter()
        .map(|id| format!("'{}'", id.into_inner()))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "SELECT DISTINCT ON (derivation_build) * FROM build_attempt \
         WHERE derivation_build IN ({in_list}) ORDER BY derivation_build, created_at DESC"
    )
}

crate::sql_fn! {
    LATEST_ATTEMPTS = || latest_attempts_sql(&[DerivationBuildId::nil(); 64]),
        params = [];
}

pub async fn latest_attempts<C: ConnectionTrait>(
    db: &C,
    shared_builds: &[DerivationBuildId],
) -> Result<std::collections::HashMap<DerivationBuildId, Model>, DbErr> {
    let rows = crate::fetch_in_chunks(shared_builds, |chunk| async move {
        Entity::find()
            .from_raw_sql(LATEST_ATTEMPTS.bind_built(latest_attempts_sql(&chunk), []))
            .all(db)
            .await
    })
    .await?;

    Ok(rows.into_iter().map(|a| (a.derivation_build, a)).collect())
}

crate::sql! {
    LATEST_ATTEMPTS_BY_DERIVATION = "SELECT DISTINCT ON (b.derivation) b.derivation AS derivation, \
             a.id AS attempt FROM build_attempt a \
             JOIN derivation_build b ON b.id = a.derivation_build \
             WHERE b.derivation = ANY($1) ORDER BY b.derivation, a.created_at DESC",
        params = [DerivationIds(64)],
        tier = Sweep;
}

pub async fn latest_attempts_by_derivation<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<std::collections::HashMap<DerivationId, BuildAttemptId>, DbErr> {
    let rows = crate::fetch_in_chunks(derivations, |chunk| async move {
        let ids: Vec<uuid::Uuid> = chunk.iter().map(|d| d.into_inner()).collect();
        db.query_all_raw(LATEST_ATTEMPTS_BY_DERIVATION.bind([ids.into()]))
            .await
    })
    .await?;

    Ok(rows
        .iter()
        .filter_map(|r| {
            Some((
                DerivationId::new(r.try_get::<uuid::Uuid>("", "derivation").ok()?),
                BuildAttemptId::new(r.try_get::<uuid::Uuid>("", "attempt").ok()?),
            ))
        })
        .collect())
}

pub async fn latest_attempt_id<C: ConnectionTrait>(
    db: &C,
    derivation_build: DerivationBuildId,
) -> Result<Option<BuildAttemptId>, DbErr> {
    Ok(latest_attempt(db, derivation_build).await?.map(|a| a.id))
}

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

pub async fn finish_latest_attempt<C: ConnectionTrait>(
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

pub async fn fail_latest_attempt<C: ConnectionTrait>(
    db: &C,
    derivation_build: DerivationBuildId,
    outcome: AttemptOutcome,
    reason: Option<AttemptFailureReason>,
    failure_message: Option<String>,
) -> Result<(), DbErr> {
    finish_latest_attempt(db, derivation_build, outcome, reason, failure_message).await
}

pub async fn succeed_latest_attempt<C: ConnectionTrait>(
    db: &C,
    derivation_build: DerivationBuildId,
    outcome: AttemptOutcome,
) -> Result<(), DbErr> {
    finish_latest_attempt(db, derivation_build, outcome, None, None).await
}

pub async fn abort_running_attempts<C: ConnectionTrait>(
    db: &C,
    shared_builds: &[DerivationBuildId],
    reason: Option<AttemptFailureReason>,
    failure_message: &str,
) -> Result<(), DbErr> {
    let now = gradient_types::now();
    crate::for_each_chunk(shared_builds, |chunk| async move {
        Entity::update_many()
            .col_expr(Column::Outcome, Expr::value(AttemptOutcome::Aborted))
            .col_expr(Column::Reason, Expr::value(reason))
            .col_expr(Column::FailureMessage, Expr::value(failure_message))
            .col_expr(Column::BuildFinishedAt, Expr::value(now))
            .filter(Column::DerivationBuild.is_in(chunk))
            .filter(Column::Outcome.eq(AttemptOutcome::Running))
            .exec(db)
            .await
    })
    .await
}

/// A streak ends at the first attempt that closed any other way. An older worker loss must not
/// count against a build that has built or failed on its own since.
pub async fn worker_loss_streaks<C: ConnectionTrait>(
    db: &C,
    shared_builds: &[DerivationBuildId],
) -> Result<std::collections::HashMap<DerivationBuildId, i64>, DbErr> {
    let rows = crate::fetch_in_chunks(shared_builds, |chunk| {
        let ids: Vec<Uuid> = chunk.iter().map(|a| a.into_inner()).collect();
        async move {
            db.query_all_raw(WORKER_LOSS_STREAKS.bind([
                ids.into(),
                (AttemptOutcome::Aborted as i32).into(),
                (AttemptFailureReason::WorkerLost as i32).into(),
            ]))
            .await
        }
    })
    .await?;

    rows.into_iter()
        .map(|r| {
            Ok((
                DerivationBuildId::new(r.try_get::<Uuid>("", "shared_build")?),
                r.try_get::<i64>("", "losses")?,
            ))
        })
        .collect()
}

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
