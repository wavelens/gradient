/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::sea_query::{Expr, ExprTrait, Func};
use sea_orm::{ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, QueryFilter, Value};

fn owned_or_builtin<C: ColumnTrait>(owner_column: C, owner: impl Into<Value>) -> Condition {
    Condition::any()
        .add(owner_column.eq(owner))
        .add(owner_column.is_null())
}

pub async fn builtin_or_owned<E, C>(
    db: &C,
    owner_column: E::Column,
    owner: impl Into<Value>,
) -> Result<Vec<E::Model>, DbErr>
where
    E: EntityTrait,
    C: ConnectionTrait,
{
    E::find()
        .filter(
            Condition::any()
                .add(owner_column.is_null())
                .add(owner_column.eq(owner)),
        )
        .all(db)
        .await
}

pub async fn by_name<E, C>(
    db: &C,
    name_column: E::Column,
    name: &str,
    owner_column: E::Column,
    owner: impl Into<Value>,
) -> Result<Option<E::Model>, DbErr>
where
    E: EntityTrait,
    C: ConnectionTrait,
{
    E::find()
        .filter(
            Condition::all()
                .add(name_column.eq(name))
                .add(owned_or_builtin(owner_column, owner)),
        )
        .one(db)
        .await
}

pub async fn name_taken<E, C>(
    db: &C,
    name_column: E::Column,
    name: &str,
    id_column: E::Column,
    except: Option<RoleId>,
    owner_column: E::Column,
    owner: impl Into<Value>,
) -> Result<bool, DbErr>
where
    E: EntityTrait,
    C: ConnectionTrait,
{
    let mut query =
        E::find().filter(Expr::expr(Func::lower(Expr::col(name_column))).eq(name.to_lowercase()));
    if let Some(role_id) = except {
        query = query.filter(id_column.ne(role_id));
    }
    let clash = query
        .filter(owned_or_builtin(owner_column, owner))
        .one(db)
        .await?;
    Ok(clash.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn built_in_role_names_are_taken_in_any_case() {
        let db = gradient_fixtures::database::TestDatabase::create().await;
        let project = ProjectId::now_v7();
        for role in [
            gradient_fixtures::rows::admin_role(),
            gradient_fixtures::rows::view_role(),
        ] {
            crate::rows::insert::<ERole, _>(&db.connection(), role)
                .await
                .expect("the built-in role is written");
        }

        for name in ["view", "VIEW", "Admin"] {
            let taken = name_taken::<ERole, _>(
                &db.connection(),
                CRole::Name,
                name,
                CRole::Id,
                None,
                CRole::Project,
                project,
            )
            .await
            .expect("the lookup runs");
            assert!(taken, "{name} must clash with a built-in role");
        }

        let free = name_taken::<ERole, _>(
            &db.connection(),
            CRole::Name,
            "releaser",
            CRole::Id,
            None,
            CRole::Project,
            project,
        )
        .await
        .expect("the lookup runs");
        assert!(!free);
        db.finish().await;
    }

    #[tokio::test]
    async fn roles_stored_without_the_view_bit_gain_it_on_upgrade() {
        use gradient_migration::{Migrator, MigratorTrait};
        use gradient_types::permissions::{
            CachePermission, Permission, cache_mask_from, cache_mask_grants, mask_from, mask_grants,
        };
        use sea_orm_migration::SchemaManager;

        let db = gradient_fixtures::database::TestDatabase::create().await;
        let deployer = crate::rows::insert::<ERole, _>(
            &db.connection(),
            MRole {
                id: RoleId::now_v7(),
                name: "deployer".into(),
                permission: mask_from(&[Permission::TriggerEvaluation]),
                ..Default::default()
            },
        )
        .await
        .expect("the custom role is written");
        let uploader = crate::rows::insert::<ECacheRole, _>(
            &db.connection(),
            MCacheRole {
                id: RoleId::now_v7(),
                name: "uploader".into(),
                permission: cache_mask_from(&[CachePermission::WriteStore]),
                ..Default::default()
            },
        )
        .await
        .expect("the custom cache role is written");

        let migration = Migrator::migrations()
            .into_iter()
            .find(|m| m.name() == "m20261128_000001_add_view_permission_bit_to_all_roles")
            .expect("the migration is registered");
        migration
            .up(&SchemaManager::new(&db.connection()))
            .await
            .expect("the migration runs");

        let deployer = db
            .find::<ERole>(deployer.id)
            .await
            .expect("the role exists");
        assert!(mask_grants(deployer.permission, Permission::ViewProject));
        assert!(mask_grants(
            deployer.permission,
            Permission::TriggerEvaluation
        ));
        let uploader = db
            .find::<ECacheRole>(uploader.id)
            .await
            .expect("the cache role exists");
        assert!(cache_mask_grants(
            uploader.permission,
            CachePermission::ViewCache
        ));
        assert!(cache_mask_grants(
            uploader.permission,
            CachePermission::WriteStore
        ));
        db.finish().await;
    }
}
