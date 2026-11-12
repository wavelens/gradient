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
        CachePermission, Permission, PermissionMask, admin_mask, cache_mask_from, mask_from,
    };
    use sea_orm_migration::SchemaManager;

    const USE_SUPERUSER_RIGHTS: &str = "m20261210_000001_add_use_superuser_rights_bit_to_existing_api_keys_not_pinned_to_cache_owned_by_superuser_with_all_project_permissions";

    const CACHE_PERMISSION_COLUMN: &str = "m20261211_000000_add_cache_permission_column_to_api_and_move_pinned_api_key_permissions_to_cache_permission";

    const VIEW_AND_READ_STORE: &str =
        "m20261130_000000_add_view_and_read_store_bits_to_existing_api_keys";
    const VIEW_PROJECT: &str =
        "m20261202_000000_add_view_project_permission_bit_to_existing_api_keys_not_pinned_to_cache";

    async fn run_migrations(conn: &sea_orm::DatabaseConnection, names: &[&str]) {
        for name in names {
            Migrator::migrations()
                .into_iter()
                .find(|m| m.name() == *name)
                .expect("the migration is registered")
                .up(&SchemaManager::new(conn))
                .await
                .expect("the migration runs");
        }
    }

    #[tokio::test]
    async fn running_m20261130_and_m20261202_migrations_in_both_orders_and_twice_gives_same_api_key_permission_bitmasks()
     {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        let owner = crate::rows::insert::<EUser, _>(&conn, rows::user())
            .await
            .expect("the owner is written");
        let cache = crate::rows::insert::<ECache, _>(&conn, rows::cache())
            .await
            .expect("the cache is written");
        let upload = cache_mask_from(&[CachePermission::WriteStore]);
        let deploy = mask_from(&[Permission::TriggerEvaluation]);
        let viewing_deploy = mask_from(&[Permission::ViewProject, Permission::TriggerEvaluation]);
        let cases: [(PermissionMask, Option<CacheId>, bool, PermissionMask); 4] = [
            (
                upload,
                Some(cache.id),
                false,
                cache_mask_from(&[
                    CachePermission::ViewCache,
                    CachePermission::ReadStore,
                    CachePermission::WriteStore,
                ]),
            ),
            (deploy, None, false, viewing_deploy),
            (viewing_deploy, None, false, viewing_deploy),
            (deploy, None, true, deploy),
        ];
        let orders: [&[&str]; 3] = [
            &[VIEW_AND_READ_STORE, VIEW_PROJECT],
            &[VIEW_PROJECT, VIEW_AND_READ_STORE],
            &[
                VIEW_AND_READ_STORE,
                VIEW_PROJECT,
                VIEW_AND_READ_STORE,
                VIEW_PROJECT,
            ],
        ];

        let mut expected: Vec<(ApiId, PermissionMask)> = Vec::new();
        for (run, order) in orders.iter().enumerate() {
            for (index, (permission, cache, managed, want)) in cases.iter().enumerate() {
                let name = format!("key-{run}-{index}");
                let row = crate::rows::insert::<EApi, _>(
                    &conn,
                    MApi {
                        id: ApiId::now_v7(),
                        owned_by: owner.id,
                        name: name.clone(),
                        key: name,
                        last_used_at: test_date(),
                        created_at: test_date(),
                        managed: *managed,
                        permission: *permission,
                        cache: *cache,
                        ..Default::default()
                    },
                )
                .await
                .expect("the key is written");
                expected.push((row.id, *want));
            }

            run_migrations(&conn, order).await;

            for (id, want) in &expected {
                let stored = db.find::<EApi>(*id).await.expect("the key exists");
                assert_eq!(stored.permission, *want, "{} after {order:?}", stored.name);
            }
        }
        db.finish().await;
    }

    #[tokio::test]
    async fn running_use_superuser_rights_migration_marks_only_unpinned_keys_of_superusers_with_all_project_permissions_and_is_idempotent()
     {
        assert_eq!(admin_mask(), 32767);
        assert_eq!(Permission::UseSuperuserRights.bit(), 65536);
        let db = TestDatabase::create().await;
        let conn = db.connection();
        let admin = crate::rows::insert::<EUser, _>(&conn, rows::superuser())
            .await
            .expect("the superuser is written");
        let member = crate::rows::insert::<EUser, _>(
            &conn,
            MUser {
                id: UserId::now_v7(),
                username: "member".into(),
                email: "member@example.com".into(),
                ..rows::user()
            },
        )
        .await
        .expect("the member is written");
        let project = crate::rows::insert::<EProject, _>(&conn, rows::project())
            .await
            .expect("the project is written");
        let full = admin_mask();
        let marked = full | Permission::UseSuperuserRights.bit();
        let cases: [(UserId, PermissionMask, Option<ProjectId>, PermissionMask); 4] = [
            (admin.id, full, None, marked),
            (admin.id, full, Some(project.id), full),
            (member.id, full, None, full),
            (
                admin.id,
                Permission::ViewProject.bit(),
                None,
                Permission::ViewProject.bit(),
            ),
        ];

        let mut expected: Vec<(ApiId, PermissionMask)> = Vec::new();
        for (index, (owner, permission, project, want)) in cases.iter().enumerate() {
            let name = format!("key-{index}");
            let row = crate::rows::insert::<EApi, _>(
                &conn,
                MApi {
                    id: ApiId::now_v7(),
                    owned_by: *owner,
                    name: name.clone(),
                    key: name,
                    last_used_at: test_date(),
                    created_at: test_date(),
                    permission: *permission,
                    project: *project,
                    ..Default::default()
                },
            )
            .await
            .expect("the key is written");
            expected.push((row.id, *want));
        }

        run_migrations(&conn, &[USE_SUPERUSER_RIGHTS, USE_SUPERUSER_RIGHTS]).await;

        for (id, want) in &expected {
            let stored = db.find::<EApi>(*id).await.expect("the key exists");
            assert_eq!(stored.permission, *want, "{}", stored.name);
        }
        db.finish().await;
    }

    #[tokio::test]
    async fn running_api_key_cache_permission_migration_moves_pinned_key_permission_to_cache_permission_keeps_unpinned_key_permission_and_is_idempotent()
     {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        let owner = crate::rows::insert::<EUser, _>(&conn, rows::user())
            .await
            .expect("the owner is written");
        let cache = crate::rows::insert::<ECache, _>(&conn, rows::cache())
            .await
            .expect("the cache is written");
        let read = cache_mask_from(&[CachePermission::ViewCache, CachePermission::ReadStore]);
        let deploy = mask_from(&[Permission::ViewProject, Permission::TriggerEvaluation]);
        let cases: [(
            PermissionMask,
            Option<CacheId>,
            PermissionMask,
            PermissionMask,
        ); 2] = [(read, Some(cache.id), 0, read), (deploy, None, deploy, 0)];

        let mut expected: Vec<(ApiId, PermissionMask, PermissionMask)> = Vec::new();
        for (index, (permission, cache, want_permission, want_cache_permission)) in
            cases.iter().enumerate()
        {
            let name = format!("key-{index}");
            let row = crate::rows::insert::<EApi, _>(
                &conn,
                MApi {
                    id: ApiId::now_v7(),
                    owned_by: owner.id,
                    name: name.clone(),
                    key: name,
                    last_used_at: test_date(),
                    created_at: test_date(),
                    permission: *permission,
                    cache: *cache,
                    ..Default::default()
                },
            )
            .await
            .expect("the key is written");
            expected.push((row.id, *want_permission, *want_cache_permission));
        }

        run_migrations(&conn, &[CACHE_PERMISSION_COLUMN, CACHE_PERMISSION_COLUMN]).await;

        for (id, want_permission, want_cache_permission) in &expected {
            let stored = db.find::<EApi>(*id).await.expect("the key exists");
            assert_eq!(
                (stored.permission, stored.cache_permission),
                (*want_permission, *want_cache_permission),
                "{}",
                stored.name
            );
        }
        db.finish().await;
    }
}
