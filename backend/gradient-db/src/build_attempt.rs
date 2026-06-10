/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_entity::build_attempt::{ActiveModel, AttemptOutcome, Column, Entity, Model};
use gradient_entity::ids::{BuildAttemptId, BuildId, DispatchedJobId};
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    QueryFilter, QueryOrder,
};

/// Open a new attempt row for `build` under `dispatched_job`.
pub async fn open_attempt<C: ConnectionTrait>(
    db: &C,
    build: BuildId,
    dispatched_job: DispatchedJobId,
    substitute: bool,
    build_context: serde_json::Value,
) -> Result<Model, DbErr> {
    ActiveModel {
        id: Set(BuildAttemptId::now_v7()),
        build: Set(build),
        dispatched_job: Set(dispatched_job),
        substitute: Set(substitute),
        outcome: Set(AttemptOutcome::Running),
        build_context: Set(build_context),
        created_at: Set(gradient_types::now()),
        ..Default::default()
    }
    .insert(db)
    .await
}

/// Most recent attempt for a build (by created_at desc), if any.
pub async fn latest_attempt<C: ConnectionTrait>(
    db: &C,
    build: BuildId,
) -> Result<Option<Model>, DbErr> {
    Entity::find()
        .filter(Column::Build.eq(build))
        .order_by_desc(Column::CreatedAt)
        .one(db)
        .await
}

/// The log id to read/finalize for a build: its latest attempt's `log_id`,
/// falling back to the build id (mirrors the old `build.log_id.unwrap_or(id)`).
pub async fn latest_attempt_log_id<C: ConnectionTrait>(
    db: &C,
    build: BuildId,
) -> Result<BuildId, DbErr> {
    Ok(latest_attempt(db, build)
        .await?
        .and_then(|a| a.log_id)
        .unwrap_or(build))
}

/// The worker that ran the build's latest attempt (via its dispatched_job).
pub async fn latest_attempt_worker<C: ConnectionTrait>(
    db: &C,
    build: BuildId,
) -> Result<Option<String>, DbErr> {
    let Some(att) = latest_attempt(db, build).await? else {
        return Ok(None);
    };

    let job = gradient_entity::dispatched_job::Entity::find_by_id(att.dispatched_job)
        .one(db)
        .await?;

    Ok(job.map(|j| j.worker_id))
}

/// Stamp `build_started_at` on the latest attempt when its build enters Building.
pub async fn stamp_attempt_started<C: ConnectionTrait>(
    db: &C,
    build: BuildId,
    now: NaiveDateTime,
) -> Result<(), DbErr> {
    if let Some(att) = latest_attempt(db, build).await?
        && att.build_started_at.is_none()
    {
        let mut a = att.into_active_model();
        a.build_started_at = Set(Some(now));
        a.update(db).await?;
    }

    Ok(())
}

/// Stamp elapsed `build_time_ms` on the latest attempt (only when leaving Building).
pub async fn finalize_attempt_timing<C: ConnectionTrait>(
    db: &C,
    build: BuildId,
    now: NaiveDateTime,
) -> Result<(), DbErr> {
    if let Some(att) = latest_attempt(db, build).await?
        && att.build_time_ms.is_none()
    {
        let started = att.build_started_at.unwrap_or(att.created_at);
        let elapsed_ms = (now - started).num_milliseconds().max(0);
        let mut a = att.into_active_model();
        a.build_time_ms = Set(Some(elapsed_ms));
        a.update(db).await?;
    }

    Ok(())
}

/// Stamp `build_finished_at` on the latest attempt for any terminal status incl. Substituted.
pub async fn stamp_attempt_finished<C: ConnectionTrait>(
    db: &C,
    build: BuildId,
    now: NaiveDateTime,
) -> Result<(), DbErr> {
    if let Some(att) = latest_attempt(db, build).await?
        && att.build_finished_at.is_none()
    {
        let mut a = att.into_active_model();
        a.build_finished_at = Set(Some(now));
        a.update(db).await?;
    }

    Ok(())
}
