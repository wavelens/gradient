/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::{DbContext, fetch_in_chunks};
use anyhow::{Context, Result};
use gradient_types::*;
use sea_orm::sea_query::{FromValueTuple, LockType};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, Iterable, ModelTrait, PrimaryKeyToColumn,
    PrimaryKeyTrait, QueryFilter, QuerySelect, Value,
};
use sea_orm_migration::prelude::*;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;

type Key<E> = <<E as EntityTrait>::PrimaryKey as PrimaryKeyTrait>::ValueType;

pub async fn by_id<E, C>(db: &C, id: Key<E>) -> Result<Option<E::Model>, DbErr>
where
    E: EntityTrait,
    C: ConnectionTrait,
{
    E::find_by_id(id).one(db).await
}

pub async fn by_id_for_delete<E, C>(db: &C, id: Key<E>) -> Result<Option<E::Model>, DbErr>
where
    E: EntityTrait,
    C: ConnectionTrait,
{
    E::find_by_id(id).lock_exclusive().one(db).await
}

pub async fn lock_for_admin_change<E, C>(db: &C, id: Key<E>) -> Result<Option<E::Model>, DbErr>
where
    E: EntityTrait,
    C: ConnectionTrait,
{
    E::find_by_id(id).lock(LockType::NoKeyUpdate).one(db).await
}

pub async fn by_ids<E, C>(db: &C, ids: &[Key<E>]) -> Result<HashMap<Key<E>, E::Model>, DbErr>
where
    E: EntityTrait,
    C: ConnectionTrait,
    Key<E>: Clone + Eq + Hash + Into<Value>,
{
    let mut seen = HashSet::new();
    let unique: Vec<Key<E>> = ids
        .iter()
        .filter(|id| seen.insert((*id).clone()))
        .cloned()
        .collect();
    let rows = fetch_in_chunks(&unique, |chunk| async move {
        let column = E::PrimaryKey::iter()
            .next()
            .expect("an entity has a primary key")
            .into_column();
        E::find().filter(column.is_in(chunk)).all(db).await
    })
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| (Key::<E>::from_value_tuple(row.get_primary_key_value()), row))
        .collect())
}

pub async fn get_project_by_name(
    ctx: &DbContext,
    user_id: UserId,
    name: String,
) -> Result<Option<MProject>> {
    EProject::find()
        .join_rev(
            JoinType::InnerJoin,
            EProjectAccess::belongs_to(gradient_entity::project::Entity)
                .from(CProjectAccess::Project)
                .to(CProject::Id)
                .into(),
        )
        .filter(
            Condition::all()
                .add(CProjectAccess::User.eq(user_id))
                .add(CProject::Name.eq(name)),
        )
        .one(&ctx.web_db)
        .await
        .context("query project")
}

pub async fn get_any_project_by_name(ctx: &DbContext, name: String) -> Result<Option<MProject>> {
    EProject::find()
        .filter(CProject::Name.eq(name))
        .one(&ctx.web_db)
        .await
        .context("query project")
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
            .context("query task")?
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
            .context("query task")?
            .map(|p| (o, p))),
        None => Ok(None),
    }
}

pub async fn get_any_cache_by_name(ctx: &DbContext, name: String) -> Result<Option<MCache>> {
    ECache::find()
        .filter(CCache::Name.eq(name))
        .one(&ctx.web_db)
        .await
        .context("query cache")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IN_CHUNK_SIZE;
    use gradient_entity::ids::UserId;
    use gradient_entity::user;
    use gradient_fixtures::mock::{empty_db, mock_db, statements};
    use gradient_fixtures::rows;

    #[tokio::test]
    async fn by_ids_of_an_empty_list_runs_no_query() {
        let db = empty_db();
        let found = by_ids::<user::Entity, _>(&db, &[]).await.unwrap();
        assert!(found.is_empty());
        assert!(db.into_transaction_log().is_empty());
    }

    #[tokio::test]
    async fn by_ids_keys_the_rows_by_id_and_asks_for_each_id_once() {
        let user = rows::user();
        let db = mock_db()
            .append_query_results([vec![user.clone()]])
            .into_connection();
        let found = by_ids::<user::Entity, _>(&db, &[user.id, user.id])
            .await
            .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[&user.id].username, user.username);
        assert_eq!(statements(db.into_transaction_log()).len(), 1);
    }

    #[tokio::test]
    async fn by_ids_beyond_one_chunk_runs_one_query_per_chunk() {
        let db = mock_db()
            .append_query_results([Vec::<user::Model>::new(), Vec::new()])
            .into_connection();
        let ids: Vec<UserId> = (0..=IN_CHUNK_SIZE).map(|_| UserId::now_v7()).collect();
        let found = by_ids::<user::Entity, _>(&db, &ids).await.unwrap();
        assert!(found.is_empty());
        assert_eq!(statements(db.into_transaction_log()).len(), 2);
    }

    #[tokio::test]
    async fn test_by_id_and_by_ids_return_rows_by_primary_key_dedupe_ids_and_skip_unknown() {
        let db = gradient_fixtures::database::TestDatabase::create().await;
        let conn = db.connection();
        let mut accounts = Vec::new();
        for username in ["alice", "bob"] {
            let account = user::Model {
                id: UserId::now_v7(),
                username: username.into(),
                name: username.into(),
                email: format!("{username}@example.com"),
                ..rows::user()
            };
            accounts.push(
                crate::rows::insert::<user::Entity, _>(&conn, account)
                    .await
                    .expect("the account is written"),
            );
        }
        let (alice, bob) = (accounts[0].id, accounts[1].id);

        let found = by_ids::<user::Entity, _>(&conn, &[alice, alice, UserId::now_v7()])
            .await
            .unwrap();
        let one = by_id::<user::Entity, _>(&conn, bob).await.unwrap();
        let none = by_id::<user::Entity, _>(&conn, UserId::now_v7())
            .await
            .unwrap();

        assert_eq!(found.keys().collect::<Vec<_>>(), [&alice]);
        assert_eq!(found[&alice].username, "alice");
        assert_eq!(one.map(|u| u.username).as_deref(), Some("bob"));
        assert!(none.is_none());
        db.finish().await;
    }
}
