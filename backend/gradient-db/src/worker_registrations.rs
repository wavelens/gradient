/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::{dispatched_job, worker_connection, worker_sample};
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect,
};

pub async fn by_worker_id<C: ConnectionTrait>(
    db: &C,
    worker_id: &str,
) -> Result<Option<MWorkerRegistration>, DbErr> {
    EWorkerRegistration::find()
        .filter(CWorkerRegistration::WorkerId.eq(worker_id))
        .one(db)
        .await
}

pub async fn of_project<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<MWorkerRegistration>, DbErr> {
    EWorkerRegistration::find()
        .filter(CWorkerRegistration::PeerId.eq(project))
        .all(db)
        .await
}

pub async fn by_project_and_worker_id<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    worker_id: &str,
) -> Result<Option<MWorkerRegistration>, DbErr> {
    EWorkerRegistration::find()
        .filter(CWorkerRegistration::PeerId.eq(project))
        .filter(CWorkerRegistration::WorkerId.eq(worker_id))
        .one(db)
        .await
}

pub async fn is_gradient_ci_worker<C: ConnectionTrait>(
    db: &C,
    worker_id: &str,
) -> Result<bool, DbErr> {
    Ok(EWorkerRegistration::find()
        .filter(CWorkerRegistration::WorkerId.eq(worker_id))
        .filter(CWorkerRegistration::GradientCi.eq(true))
        .one(db)
        .await?
        .is_some())
}

pub async fn delete_by_project_and_worker_id<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    worker_id: &str,
) -> Result<u64, DbErr> {
    Ok(EWorkerRegistration::delete_many()
        .filter(CWorkerRegistration::PeerId.eq(project))
        .filter(CWorkerRegistration::WorkerId.eq(worker_id))
        .exec(db)
        .await?
        .rows_affected)
}

pub async fn oldest_samples<C: ConnectionTrait>(
    db: &C,
    worker_id: &str,
    limit: u64,
) -> Result<Vec<worker_sample::Model>, DbErr> {
    worker_sample::Entity::find()
        .filter(worker_sample::Column::WorkerId.eq(worker_id))
        .order_by_asc(worker_sample::Column::At)
        .limit(limit)
        .all(db)
        .await
}

pub async fn latest_connections<C: ConnectionTrait>(
    db: &C,
    worker_id: &str,
    limit: u64,
) -> Result<Vec<worker_connection::Model>, DbErr> {
    worker_connection::Entity::find()
        .filter(worker_connection::Column::WorkerId.eq(worker_id))
        .order_by_desc(worker_connection::Column::ConnectedAt)
        .limit(limit)
        .all(db)
        .await
}

pub async fn dispatched_job_count<C: ConnectionTrait>(
    db: &C,
    worker_id: &str,
    project: ProjectId,
) -> Result<u64, DbErr> {
    dispatched_job::Entity::find()
        .filter(dispatched_job::Column::WorkerId.eq(worker_id))
        .filter(dispatched_job::Column::Project.eq(project))
        .count(db)
        .await
}

pub async fn by_worker_id_or_gradient_ci_project<C: ConnectionTrait>(
    db: &C,
    worker_id: &str,
    project: Option<ProjectId>,
) -> Result<Option<MWorkerRegistration>, DbErr> {
    let same_worker = Condition::any().add(CWorkerRegistration::WorkerId.eq(worker_id));
    let registrations = match project {
        None => same_worker,
        Some(project) => same_worker.add(
            Condition::all()
                .add(CWorkerRegistration::PeerId.eq(project))
                .add(CWorkerRegistration::GradientCi.eq(true)),
        ),
    };

    EWorkerRegistration::find()
        .filter(registrations)
        .one(db)
        .await
}

pub async fn insert<C: ConnectionTrait>(
    db: &C,
    registration: MWorkerRegistration,
) -> Result<MWorkerRegistration, DbErr> {
    registration.into_active_model().insert(db).await
}

pub async fn open_dispatched_jobs<C: ConnectionTrait>(
    db: &C,
    limit: u64,
) -> Result<Vec<dispatched_job::Model>, DbErr> {
    dispatched_job::Entity::find()
        .filter(dispatched_job::Column::FinishedAt.is_null())
        .order_by_desc(dispatched_job::Column::DispatchedAt)
        .limit(limit)
        .all(db)
        .await
}

pub async fn phases_of_dispatched_job<C: ConnectionTrait>(
    db: &C,
    job: gradient_entity::ids::DispatchedJobId,
) -> Result<Vec<gradient_entity::dispatched_job_phase::Model>, DbErr> {
    use gradient_entity::dispatched_job_phase::{Column, Entity};

    Entity::find()
        .filter(Column::DispatchedJob.eq(job))
        .order_by_asc(Column::Seq)
        .all(db)
        .await
}

fn scoring_summary_sql(window_hours: i64, limit: u64, project_filter: Option<&str>) -> String {
    let mut clauses = vec![format!(
        "dispatched_at >= (now() AT TIME ZONE 'UTC') - interval '{window_hours} hours'"
    )];

    if let Some(list) = project_filter {
        clauses.push(format!("project IN ({list})"));
    }

    format!(
        "SELECT score, score_breakdown FROM dispatched_job WHERE {} \
         ORDER BY dispatched_at DESC LIMIT {limit}",
        clauses.join(" AND ")
    )
}

crate::sql_fn! {
    SCORING_SUMMARY = || scoring_summary_sql(
        24,
        2000,
        Some("'11111111-1111-1111-1111-111111111111'"),
    ),
        params = [];
}

pub struct DispatchScore {
    pub score: f64,
    pub score_breakdown: Option<serde_json::Value>,
}

pub async fn recent_scores<C: ConnectionTrait>(
    db: &C,
    window_hours: i64,
    limit: u64,
    project_filter: Option<&str>,
) -> Result<Vec<DispatchScore>, DbErr> {
    let rows = db
        .query_all_raw(
            SCORING_SUMMARY
                .bind_built(scoring_summary_sql(window_hours, limit, project_filter), []),
        )
        .await?;

    Ok(rows
        .iter()
        .map(|r| DispatchScore {
            score: r.try_get("", "score").unwrap_or(0.0),
            score_breakdown: r.try_get("", "score_breakdown").ok(),
        })
        .collect())
}
