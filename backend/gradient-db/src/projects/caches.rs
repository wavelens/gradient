/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::ids::{CacheId, ProjectId};
use gradient_types::{CProjectCache, EProjectCache, MProjectCache};
use sea_orm::{ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, QueryFilter};

pub async fn project_has_writable_cache<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<bool, sea_orm::DbErr> {
    Ok(!crate::cache_paths::project_write_caches(db, project)
        .await?
        .is_empty())
}

pub async fn subscriptions_of<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<MProjectCache>, DbErr> {
    EProjectCache::find()
        .filter(CProjectCache::Project.eq(project))
        .all(db)
        .await
}

pub async fn subscription<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    cache: CacheId,
) -> Result<Option<MProjectCache>, DbErr> {
    EProjectCache::find()
        .filter(
            Condition::all()
                .add(CProjectCache::Project.eq(project))
                .add(CProjectCache::Cache.eq(cache)),
        )
        .one(db)
        .await
}

pub async fn subscriptions_to<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
) -> Result<Vec<MProjectCache>, DbErr> {
    EProjectCache::find()
        .filter(CProjectCache::Cache.eq(cache))
        .all(db)
        .await
}

pub async fn subscription_of_any<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
    projects: Vec<ProjectId>,
) -> Result<Option<MProjectCache>, DbErr> {
    EProjectCache::find()
        .filter(CProjectCache::Cache.eq(cache))
        .filter(CProjectCache::Project.is_in(projects))
        .one(db)
        .await
}

pub async fn first_subscription_of<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Option<MProjectCache>, DbErr> {
    EProjectCache::find()
        .filter(CProjectCache::Project.eq(project))
        .one(db)
        .await
}

pub async fn subscriptions_of_projects<C: ConnectionTrait>(
    db: &C,
    projects: Vec<ProjectId>,
) -> Result<Vec<MProjectCache>, DbErr> {
    EProjectCache::find()
        .filter(CProjectCache::Project.is_in(projects))
        .all(db)
        .await
}
