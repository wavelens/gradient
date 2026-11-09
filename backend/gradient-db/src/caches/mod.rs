/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod capacity;
pub mod debug_info;
pub mod demotion;
pub mod members;
pub mod reach;
pub mod upstream;
pub mod usage;

use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::sea_query::{LockType, Query};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, Select,
};

pub async fn by_name<C: ConnectionTrait>(db: &C, name: &str) -> Result<Option<MCache>, DbErr> {
    ECache::find().filter(CCache::Name.eq(name)).one(db).await
}

pub async fn by_ids<C: ConnectionTrait>(db: &C, ids: &[CacheId]) -> Result<Vec<MCache>, DbErr> {
    ECache::find()
        .filter(CCache::Id.is_in(ids.iter().copied()))
        .all(db)
        .await
}

pub async fn lock_for_admin_change<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
) -> Result<Option<MCache>, DbErr> {
    ECache::find_by_id(cache)
        .lock(LockType::NoKeyUpdate)
        .one(db)
        .await
}

pub fn ordered_by_name(condition: Condition) -> Select<ECache> {
    ECache::find()
        .filter(condition)
        .order_by_asc(CCache::DisplayName)
        .order_by_asc(CCache::Name)
        .order_by_asc(CCache::Id)
}

pub async fn all_public<C: ConnectionTrait>(db: &C) -> Result<Vec<MCache>, DbErr> {
    ECache::find().filter(CCache::Public.eq(true)).all(db).await
}

pub async fn set_active<C: ConnectionTrait>(
    db: &C,
    cache: MCache,
    active: bool,
) -> Result<MCache, DbErr> {
    let mut cache: ACache = cache.into();
    cache.active = Set(active);
    cache.update(db).await
}

pub async fn set_public<C: ConnectionTrait>(
    db: &C,
    cache: MCache,
    public: bool,
) -> Result<MCache, DbErr> {
    let mut cache: ACache = cache.into();
    cache.public = Set(public);
    cache.update(db).await
}

pub async fn access_of_user<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
    user: UserId,
) -> Result<Option<MCacheAccess>, DbErr> {
    ECacheAccess::find()
        .filter(CCacheAccess::Cache.eq(cache))
        .filter(CCacheAccess::User.eq(user))
        .one(db)
        .await
}

pub async fn among_matching<C: ConnectionTrait>(
    db: &C,
    ids: &[CacheId],
    condition: Condition,
) -> Result<Vec<MCache>, DbErr> {
    ECache::find()
        .filter(CCache::Id.is_in(ids.iter().copied()))
        .filter(condition)
        .all(db)
        .await
}

pub fn public_or(visible: Option<Condition>) -> Condition {
    let public = Condition::any().add(CCache::Public.eq(true));
    match visible {
        Some(visible) => public.add(visible),
        None => public,
    }
}

pub fn subscribed_caches_of_project_visible_to_user(user: UserId, project: ProjectId) -> Condition {
    let reachable_project = Query::select()
        .column(CProjectAccess::Project)
        .from(gradient_entity::project_access::Entity)
        .and_where(CProjectAccess::User.eq(user))
        .and_where(CProjectAccess::Project.eq(project))
        .to_owned();
    let subscribed = Query::select()
        .column(CProjectCache::Cache)
        .from(gradient_entity::project_cache::Entity)
        .and_where(CProjectCache::Project.in_subquery(reachable_project))
        .to_owned();
    Condition::all().add(CCache::Id.in_subquery(subscribed))
}

pub fn visible_to_user(user: UserId) -> Condition {
    let member_of = Query::select()
        .column(CCacheAccess::Cache)
        .from(gradient_entity::cache_access::Entity)
        .and_where(CCacheAccess::User.eq(user))
        .to_owned();
    let reachable_projects = Query::select()
        .column(CProjectAccess::Project)
        .from(gradient_entity::project_access::Entity)
        .and_where(CProjectAccess::User.eq(user))
        .to_owned();
    let subscribed_by_reachable_project = Query::select()
        .column(CProjectCache::Cache)
        .from(gradient_entity::project_cache::Entity)
        .and_where(CProjectCache::Project.in_subquery(reachable_projects))
        .to_owned();
    Condition::any()
        .add(CCache::CreatedBy.eq(user))
        .add(CCache::Id.in_subquery(member_of))
        .add(CCache::Id.in_subquery(subscribed_by_reachable_project))
}
