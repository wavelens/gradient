/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, QueryFilter};

pub async fn of_project<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<MCacheSubscriptionRequest>, DbErr> {
    ECacheSubscriptionRequest::find()
        .filter(CCacheSubscriptionRequest::Project.eq(project))
        .all(db)
        .await
}

pub async fn by_project_and_cache<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    cache: CacheId,
) -> Result<Option<MCacheSubscriptionRequest>, DbErr> {
    ECacheSubscriptionRequest::find()
        .filter(
            Condition::all()
                .add(CCacheSubscriptionRequest::Project.eq(project))
                .add(CCacheSubscriptionRequest::Cache.eq(cache)),
        )
        .one(db)
        .await
}

pub async fn of_cache<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
) -> Result<Vec<MCacheSubscriptionRequest>, DbErr> {
    ECacheSubscriptionRequest::find()
        .filter(CCacheSubscriptionRequest::Cache.eq(cache))
        .all(db)
        .await
}
