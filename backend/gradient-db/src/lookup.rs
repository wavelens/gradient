/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::DbContext;
use anyhow::{Context, Result};
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};
use sea_orm_migration::prelude::*;

pub async fn get_project_by_name(
    ctx: &DbContext,
    user_id: UserId,
    name: String,
) -> Result<Option<MProject>> {
    EProject::find()
        .join_rev(
            JoinType::InnerJoin,
            EProjectUser::belongs_to(gradient_entity::project::Entity)
                .from(CProjectUser::Project)
                .to(CProject::Id)
                .into(),
        )
        .filter(
            Condition::all()
                .add(CProjectUser::User.eq(user_id))
                .add(CProject::Name.eq(name)),
        )
        .one(&ctx.web_db)
        .await
        .context("Failed to query project")
}

pub async fn get_any_project_by_name(ctx: &DbContext, name: String) -> Result<Option<MProject>> {
    EProject::find()
        .filter(CProject::Name.eq(name))
        .one(&ctx.web_db)
        .await
        .context("Failed to query project")
}

pub async fn get_task_by_name(
    ctx: &DbContext,
    user_id: UserId,
    project_name: String,
    task_name: String,
) -> Result<Option<(MProject, MTask)>> {
    match get_project_by_name(ctx, user_id, project_name).await? {
        Some(o) => Ok(ETask::find()
            .filter(CTask::Project.eq(o.id))
            .filter(CTask::Name.eq(task_name))
            .one(&ctx.web_db)
            .await
            .context("Failed to query task")?
            .map(|p| (o, p))),
        None => Ok(None),
    }
}

pub async fn get_any_task_by_name(
    ctx: &DbContext,
    project_name: String,
    task_name: String,
) -> Result<Option<(MProject, MTask)>> {
    match get_any_project_by_name(ctx, project_name).await? {
        Some(o) => Ok(ETask::find()
            .filter(CTask::Project.eq(o.id))
            .filter(CTask::Name.eq(task_name))
            .one(&ctx.web_db)
            .await
            .context("Failed to query task")?
            .map(|p| (o, p))),
        None => Ok(None),
    }
}

pub async fn get_cache_by_name(
    ctx: &DbContext,
    user_id: UserId,
    name: String,
) -> Result<Option<MCache>> {
    ECache::find()
        .filter(
            Condition::all()
                .add(CCache::CreatedBy.eq(user_id))
                .add(CCache::Name.eq(name)),
        )
        .one(&ctx.web_db)
        .await
        .context("Failed to query cache")
}

pub async fn get_any_cache_by_name(ctx: &DbContext, name: String) -> Result<Option<MCache>> {
    ECache::find()
        .filter(CCache::Name.eq(name))
        .one(&ctx.web_db)
        .await
        .context("Failed to query cache")
}
