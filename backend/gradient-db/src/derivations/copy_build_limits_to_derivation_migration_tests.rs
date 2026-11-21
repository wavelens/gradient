/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::ids::{DerivationBuildId, DerivationId};
use gradient_entity::{derivation, derivation_build};
use gradient_fixtures::database::TestDatabase;
use gradient_migration::{Migrator, MigratorTrait};
use sea_orm::sea_query::{Alias, Expr};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QuerySelect};

const LIMITS_MIGRATION: &str = "m20261230_000001_move_timeout_secs_and_max_silent_secs_to_derivation_and_add_derivation_file_hash_modulo";

async fn revert_migrations_through_build_limits_move_to_derivation(db: &DatabaseConnection) {
    let migrations = Migrator::migrations();
    let steps = migrations.len()
        - migrations
            .iter()
            .position(|m| m.name() == LIMITS_MIGRATION)
            .expect("the limits migration is registered");
    Migrator::down(db, Some(u32::try_from(steps).expect("step count")))
        .await
        .expect("the limits migration reverts");
}

async fn read_derivation_build_timeout_secs_and_max_silent_secs_by_column_name(
    db: &DatabaseConnection,
    derivation: DerivationId,
) -> (Option<i64>, Option<i64>) {
    derivation_build::Entity::find()
        .select_only()
        .expr_as(Expr::col(Alias::new("timeout_secs")), "timeout_secs")
        .expr_as(Expr::col(Alias::new("max_silent_secs")), "max_silent_secs")
        .filter(derivation_build::Column::Derivation.eq(derivation))
        .into_tuple()
        .one(db)
        .await
        .expect("derivation build query")
        .expect("derivation build row")
}

async fn is_stored_hash_modulo_column_missing_from_derivation_table_after_revert(
    db: &DatabaseConnection,
    derivation: DerivationId,
) -> bool {
    derivation::Entity::find()
        .select_only()
        .expr_as(
            Expr::col(Alias::new(
                "serialized_derivation_hash_for_dependent_output_paths",
            )),
            "value",
        )
        .filter(derivation::Column::Id.eq(derivation))
        .into_tuple::<Option<String>>()
        .one(db)
        .await
        .is_err()
}

async fn insert_derivation_with_build_limits(
    db: &DatabaseConnection,
    name: &str,
    timeout_secs: Option<i64>,
    max_silent_secs: Option<i64>,
) -> DerivationId {
    let id = DerivationId::now_v7();
    crate::rows::insert::<derivation::Entity, _>(
        db,
        derivation::Model {
            timeout_secs,
            max_silent_secs,
            ..gradient_fixtures::rows::derivation_named(id, name)
        },
    )
    .await
    .expect("derivation");
    crate::rows::insert::<derivation_build::Entity, _>(
        db,
        derivation_build::Model {
            id: DerivationBuildId::now_v7(),
            derivation: id,
            created_at: gradient_fixtures::test_date(),
            updated_at: gradient_fixtures::test_date(),
            ..Default::default()
        },
    )
    .await
    .expect("derivation build");
    id
}

#[tokio::test]
async fn build_limits_migration_down_up_twice_copies_limits_to_derivation_build_and_keeps_derivation_limits_with_nulls()
 {
    let database = TestDatabase::create().await;
    let db = database.connection();
    let limited = insert_derivation_with_build_limits(&db, "limited", Some(3600), Some(600)).await;
    let unlimited = insert_derivation_with_build_limits(&db, "unlimited", None, None).await;
    assert_eq!(
        read_derivation_build_timeout_secs_and_max_silent_secs_by_column_name(&db, limited).await,
        (None, None),
        "before: derivation_build(limited)"
    );

    for run in 1..=2 {
        revert_migrations_through_build_limits_move_to_derivation(&db).await;
        assert_eq!(
            read_derivation_build_timeout_secs_and_max_silent_secs_by_column_name(&db, limited)
                .await,
            (Some(3600), Some(600)),
            "down {run}: derivation_build(limited)"
        );
        assert_eq!(
            read_derivation_build_timeout_secs_and_max_silent_secs_by_column_name(&db, unlimited)
                .await,
            (None, None),
            "down {run}: derivation_build(unlimited)"
        );
        assert!(
            is_stored_hash_modulo_column_missing_from_derivation_table_after_revert(&db, limited)
                .await,
            "down {run}: column for hash modulo in the derivation table"
        );

        Migrator::up(&db, None)
            .await
            .expect("the limits migration runs");
        assert_eq!(
            read_derivation_build_timeout_secs_and_max_silent_secs_by_column_name(&db, limited)
                .await,
            (Some(3600), Some(600)),
            "up {run}: derivation_build(limited) keeps its columns"
        );
        let limits_of = |row: Option<derivation::Model>| {
            let row = row.expect("derivation row");
            (row.timeout_secs, row.max_silent_secs)
        };
        let limited_row = database.find::<derivation::Entity>(limited).await;
        assert_eq!(limits_of(limited_row), (Some(3600), Some(600)), "up {run}");
        let unlimited_row = database.find::<derivation::Entity>(unlimited).await;
        assert_eq!(limits_of(unlimited_row), (None, None), "up {run}");
        assert_eq!(
            database
                .find::<derivation::Entity>(limited)
                .await
                .expect("derivation row")
                .serialized_derivation_hash_for_dependent_output_paths,
            None,
            "up {run}: derived hash modulo column for reverted rows"
        );
    }
    database.finish().await;
}
