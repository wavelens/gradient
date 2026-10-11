/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::{dispatched_job, worker_connection, worker_sample};
use gradient_types::*;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect,
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
