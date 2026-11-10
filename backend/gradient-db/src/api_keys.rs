/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    QueryFilter, QueryOrder,
};

pub async fn of_user<C: ConnectionTrait>(db: &C, user: UserId) -> Result<Vec<MApi>, DbErr> {
    EApi::find()
        .filter(CApi::OwnedBy.eq(user))
        .order_by_desc(CApi::CreatedAt)
        .all(db)
        .await
}

pub async fn by_owner_and_name<C: ConnectionTrait>(
    db: &C,
    owner: UserId,
    name: &str,
) -> Result<Option<MApi>, DbErr> {
    EApi::find()
        .filter(
            Condition::all()
                .add(CApi::OwnedBy.eq(owner))
                .add(CApi::Name.eq(name)),
        )
        .one(db)
        .await
}

pub async fn other_with_name<C: ConnectionTrait>(
    db: &C,
    owner: UserId,
    name: &str,
    api_key: ApiId,
) -> Result<Option<MApi>, DbErr> {
    EApi::find()
        .filter(
            Condition::all()
                .add(CApi::OwnedBy.eq(owner))
                .add(CApi::Name.eq(name))
                .add(CApi::Id.ne(api_key)),
        )
        .one(db)
        .await
}

pub async fn by_key_hash<C: ConnectionTrait>(
    db: &C,
    key_hash: &str,
) -> Result<Option<MApi>, DbErr> {
    EApi::find().filter(CApi::Key.eq(key_hash)).one(db).await
}

pub async fn stamp_last_used<C: ConnectionTrait>(
    db: &C,
    api_key: MApi,
    at: NaiveDateTime,
) -> Result<(), DbErr> {
    let mut active = api_key.into_active_model();
    active.last_used_at = Set(at);
    active.save(db).await?;
    Ok(())
}

pub async fn revoke<C: ConnectionTrait>(
    db: &C,
    api_key: MApi,
    at: NaiveDateTime,
) -> Result<MApi, DbErr> {
    let mut active = api_key.into_active_model();
    active.revoked_at = Set(Some(at));
    active.update(db).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::{rows, test_date};
    use gradient_migration::{Migrator, MigratorTrait};
    use gradient_types::permissions::{
        CachePermission, Permission, PermissionMask, cache_mask_from, cache_mask_to_vec, mask_from,
        mask_to_vec,
    };
    use sea_orm_migration::SchemaManager;

    fn key(name: &str, permission: PermissionMask, cache: Option<CacheId>) -> MApi {
        MApi {
            id: ApiId::now_v7(),
            owned_by: rows::user().id,
            name: name.into(),
            key: name.into(),
            permission,
            cache,
            last_used_at: test_date(),
            created_at: test_date(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn migration_adds_view_project_permission_to_non_cache_api_keys_and_keeps_cache_pinned_permission_mask_unchanged()
     {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        crate::rows::insert::<EUser, _>(&conn, rows::user())
            .await
            .expect("the owner is written");
        let cache = crate::rows::insert::<ECache, _>(&conn, rows::cache())
            .await
            .expect("the cache is written");
        let deploy = mask_from(&[Permission::TriggerEvaluation]);
        let upload = cache_mask_from(&[CachePermission::WriteStore]);
        let project_key = crate::rows::insert::<EApi, _>(&conn, key("deploy", deploy, None))
            .await
            .expect("the project key is written");
        let cache_key =
            crate::rows::insert::<EApi, _>(&conn, key("upload", upload, Some(cache.id)))
                .await
                .expect("the cache key is written");

        Migrator::migrations()
            .into_iter()
            .find(|m| {
                m.name()
                    == "m20261202_000000_add_view_project_permission_bit_to_existing_api_keys_not_pinned_to_cache"
            })
            .expect("the migration is registered")
            .up(&SchemaManager::new(&conn))
            .await
            .expect("the migration runs");

        let project_key = db
            .find::<EApi>(project_key.id)
            .await
            .expect("the project key exists");
        let cache_key = db
            .find::<EApi>(cache_key.id)
            .await
            .expect("the cache key exists");
        assert_eq!(
            project_key.permission,
            deploy | Permission::ViewProject.bit()
        );
        assert_eq!(cache_key.permission, upload);
        db.finish().await;
    }

    #[tokio::test]
    async fn migration_adds_missing_api_key_permission_bits_to_existing_api_keys() {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        let owner = crate::rows::insert::<EUser, _>(&conn, rows::user())
            .await
            .expect("the owner is written");
        let cache = crate::rows::insert::<ECache, _>(&conn, rows::cache())
            .await
            .expect("the cache is written");
        let key = |name: &str, permission: i64, cache: Option<CacheId>, managed: bool| MApi {
            id: ApiId::now_v7(),
            owned_by: owner.id,
            name: name.into(),
            key: name.into(),
            last_used_at: test_date(),
            created_at: test_date(),
            managed,
            permission,
            cache,
            ..Default::default()
        };
        let push = key(
            "push",
            cache_mask_from(&[CachePermission::WriteStore]),
            Some(cache.id),
            false,
        );
        let deploy = key(
            "deploy",
            mask_from(&[Permission::TriggerEvaluation]),
            None,
            false,
        );
        let declared = key(
            "declared",
            mask_from(&[Permission::TriggerEvaluation]),
            None,
            true,
        );
        for row in [&push, &deploy, &declared] {
            crate::rows::insert::<EApi, _>(&conn, row.clone())
                .await
                .expect("the key is written");
        }

        let migration = Migrator::migrations()
            .into_iter()
            .find(|m| {
                m.name() == "m20261130_000000_add_view_and_read_store_bits_to_existing_api_keys"
            })
            .expect("the migration is registered");
        for _ in 0..2 {
            migration
                .up(&SchemaManager::new(&conn))
                .await
                .expect("the migration runs");
        }

        let permission_of = async |row: &MApi| {
            db.find::<EApi>(row.id)
                .await
                .expect("the key exists")
                .permission
        };
        assert_eq!(
            cache_mask_to_vec(permission_of(&push).await),
            [
                CachePermission::ViewCache,
                CachePermission::ReadStore,
                CachePermission::WriteStore
            ]
        );
        assert_eq!(
            mask_to_vec(permission_of(&deploy).await),
            [Permission::ViewProject, Permission::TriggerEvaluation]
        );
        assert_eq!(permission_of(&declared).await, declared.permission);
        db.finish().await;
    }
}
