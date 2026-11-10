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

use gradient_entity::retired_cache_signing_key;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::sea_query::{Expr, Query};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, QueryFilter,
    QueryOrder, Select, TransactionTrait,
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

pub async fn insert_old_cache_key_row_replace_key_pair_and_move_signatures_to_previous_column<
    C: TransactionTrait<Transaction = sea_orm::DatabaseTransaction>,
>(
    db: &C,
    cache: MCache,
    retired_public_key: String,
    (private_key, public_key): (String, String),
) -> Result<MCache, DbErr> {
    let tx = db.begin().await?;
    let now = gradient_types::now();
    let created_at = list_retired_signing_keys_for_cache_newest_retirement_first(&tx, cache.id)
        .await?
        .first()
        .map_or(cache.created_at, |newest| newest.retired_at);
    retired_cache_signing_key::ActiveModel {
        id: Set(RetiredCacheSigningKeyId::now_v7()),
        cache: Set(cache.id),
        public_key: Set(retired_public_key),
        created_at: Set(created_at),
        retired_at: Set(now),
    }
    .insert(&tx)
    .await?;

    ECachedPathSignature::update_many()
        .col_expr(
            CCachedPathSignature::PreviousSigningKeySignature,
            Expr::cust("coalesce(signature, previous_signing_key_signature)"),
        )
        .col_expr(
            CCachedPathSignature::Signature,
            Expr::value(Option::<Vec<u8>>::None),
        )
        .filter(CCachedPathSignature::Cache.eq(cache.id))
        .exec(&tx)
        .await?;

    let mut cache: ACache = cache.into();
    cache.private_key = Set(private_key);
    cache.public_key = Set(public_key);
    let cache = cache.update(&tx).await?;
    tx.commit().await?;
    Ok(cache)
}

pub async fn list_retired_signing_keys_for_cache_newest_retirement_first<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
) -> Result<Vec<retired_cache_signing_key::Model>, DbErr> {
    retired_cache_signing_key::Entity::find()
        .filter(retired_cache_signing_key::Column::Cache.eq(cache))
        .order_by_desc(retired_cache_signing_key::Column::RetiredAt)
        .all(db)
        .await
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

pub fn subscribed_binary_caches_of_projects_visible_to_user(
    user: UserId,
    project: Option<ProjectId>,
) -> Condition {
    let mut reachable_projects = Query::select()
        .column(CProjectAccess::Project)
        .from(gradient_entity::project_access::Entity)
        .and_where(CProjectAccess::User.eq(user))
        .to_owned();
    if let Some(project) = project {
        reachable_projects.and_where(CProjectAccess::Project.eq(project));
    }
    let subscribed = Query::select()
        .column(CProjectCache::Cache)
        .from(gradient_entity::project_cache::Entity)
        .and_where(CProjectCache::Project.in_subquery(reachable_projects))
        .to_owned();
    Condition::all().add(CCache::Id.in_subquery(subscribed))
}

pub fn binary_caches_created_by_user_or_with_direct_cache_role_for_user(user: UserId) -> Condition {
    let member_of = Query::select()
        .column(CCacheAccess::Cache)
        .from(gradient_entity::cache_access::Entity)
        .and_where(CCacheAccess::User.eq(user))
        .to_owned();
    Condition::any()
        .add(CCache::CreatedBy.eq(user))
        .add(CCache::Id.in_subquery(member_of))
}

pub fn visible_to_user(user: UserId) -> Condition {
    Condition::any()
        .add(binary_caches_created_by_user_or_with_direct_cache_role_for_user(user))
        .add(subscribed_binary_caches_of_projects_visible_to_user(
            user, None,
        ))
}
