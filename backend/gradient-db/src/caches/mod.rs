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
use sea_orm::sea_query::LockType;
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

pub fn created_by_or_among(creator: UserId, caches: Vec<CacheId>) -> Condition {
    Condition::any()
        .add(CCache::CreatedBy.eq(creator))
        .add(CCache::Id.is_in(caches))
}
