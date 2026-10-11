/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    QueryFilter,
};

pub async fn project_request<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
    project: ProjectId,
) -> Result<Option<MTeamProjectRequest>, DbErr> {
    ETeamProjectRequest::find()
        .filter(CTeamProjectRequest::Team.eq(team))
        .filter(CTeamProjectRequest::Project.eq(project))
        .one(db)
        .await
}

pub async fn requests_of_project<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<MTeamProjectRequest>, DbErr> {
    ETeamProjectRequest::find()
        .filter(CTeamProjectRequest::Project.eq(project))
        .all(db)
        .await
}

pub async fn project_requests_of_team<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
) -> Result<Vec<MTeamProjectRequest>, DbErr> {
    ETeamProjectRequest::find()
        .filter(CTeamProjectRequest::Team.eq(team))
        .all(db)
        .await
}

pub async fn project_request_by_id<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
    id: TeamProjectRequestId,
) -> Result<Option<MTeamProjectRequest>, DbErr> {
    ETeamProjectRequest::find_by_id(id)
        .filter(CTeamProjectRequest::Team.eq(team))
        .one(db)
        .await
}

pub async fn add_project_request<C: ConnectionTrait>(
    db: &C,
    request: MTeamProjectRequest,
) -> Result<MTeamProjectRequest, DbErr> {
    request.into_active_model().insert(db).await
}

pub async fn remove_project_request<C: ConnectionTrait>(
    db: &C,
    request: MTeamProjectRequest,
) -> Result<bool, DbErr> {
    crate::rows::delete_was_present::<ETeamProjectRequest, _>(db, request).await
}

pub async fn cache_request<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
    cache: CacheId,
) -> Result<Option<MTeamCacheRequest>, DbErr> {
    ETeamCacheRequest::find()
        .filter(CTeamCacheRequest::Team.eq(team))
        .filter(CTeamCacheRequest::Cache.eq(cache))
        .one(db)
        .await
}

pub async fn requests_of_cache<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
) -> Result<Vec<MTeamCacheRequest>, DbErr> {
    ETeamCacheRequest::find()
        .filter(CTeamCacheRequest::Cache.eq(cache))
        .all(db)
        .await
}

pub async fn cache_requests_of_team<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
) -> Result<Vec<MTeamCacheRequest>, DbErr> {
    ETeamCacheRequest::find()
        .filter(CTeamCacheRequest::Team.eq(team))
        .all(db)
        .await
}

pub async fn cache_request_by_id<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
    id: TeamCacheRequestId,
) -> Result<Option<MTeamCacheRequest>, DbErr> {
    ETeamCacheRequest::find_by_id(id)
        .filter(CTeamCacheRequest::Team.eq(team))
        .one(db)
        .await
}

pub async fn add_cache_request<C: ConnectionTrait>(
    db: &C,
    request: MTeamCacheRequest,
) -> Result<MTeamCacheRequest, DbErr> {
    request.into_active_model().insert(db).await
}

pub async fn remove_cache_request<C: ConnectionTrait>(
    db: &C,
    request: MTeamCacheRequest,
) -> Result<bool, DbErr> {
    crate::rows::delete_was_present::<ETeamCacheRequest, _>(db, request).await
}
