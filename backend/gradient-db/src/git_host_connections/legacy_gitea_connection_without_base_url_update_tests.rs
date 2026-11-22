/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::*;
use gradient_entity::{git_host_connection, user};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::ids::user_id;
use gradient_migration::{Migrator, MigratorTrait};
use sea_orm::DatabaseConnection;

async fn create_test_database_with_gitea_connection_without_base_url_by_reverting_before_base_url_check_and_running_migrations_up()
-> (TestDatabase, DatabaseConnection, MGitHostConnection) {
    let database = TestDatabase::create().await;
    let db = database.connection();
    crate::rows::insert::<user::Entity, _>(&db, gradient_fixtures::rows::user())
        .await
        .expect("user");
    let migrations = Migrator::migrations();
    let position = migrations
        .iter()
        .position(|m| m.name().starts_with("m20261228_000000_"))
        .expect("migrator list includes the migration for the base_url check");
    let steps = u32::try_from(migrations.len() - position).expect("the count fits in u32");
    Migrator::down(&db, Some(steps))
        .await
        .expect("migration for the base_url check did not revert");
    let id = GitHostConnectionId::now_v7();
    let connection = crate::rows::insert::<git_host_connection::Entity, _>(
        &db,
        git_host_connection::Model {
            id,
            name: format!("connection-{id}"),
            display_name: "connection".into(),
            git_host_type: GitHostType::Gitea,
            base_url: None,
            receives_events: true,
            reports_status: true,
            created_by: user_id(),
            created_at: gradient_fixtures::test_date(),
            ..Default::default()
        },
    )
    .await
    .expect("connection");
    Migrator::up(&db, None)
        .await
        .expect("running database migrations");
    (database, db, connection)
}

async fn stored(db: &DatabaseConnection, id: GitHostConnectionId) -> Option<MGitHostConnection> {
    EGitHostConnection::find_by_id(id)
        .one(db)
        .await
        .expect("connection row loaded")
}

#[tokio::test]
async fn count_refused_signature_upgrades_gitea_connection_without_base_url() {
    let (database, db, connection) =
        create_test_database_with_gitea_connection_without_base_url_by_reverting_before_base_url_check_and_running_migrations_up().await;

    count_refused_signature(&db, connection.id)
        .await
        .expect("rejected signature count");

    let row = stored(&db, connection.id).await.expect("row");
    assert_eq!(row.refused_signature_count, 1);
    assert!(row.last_refused_signature_at.is_some());
    database.finish().await;
}

#[tokio::test]
async fn upgraded_database_gitea_connection_without_base_url_can_be_turned_off_by_writing_all_columns()
 {
    let (database, db, connection) =
        create_test_database_with_gitea_connection_without_base_url_by_reverting_before_base_url_check_and_running_migrations_up().await;

    write_all_columns_of_git_host_connection(
        &db,
        MGitHostConnection {
            receives_events: false,
            reports_status: false,
            ..connection.clone()
        },
    )
    .await
    .expect("connection is off");

    let row = stored(&db, connection.id).await.expect("row");
    assert!(!row.receives_events);
    assert!(!row.reports_status);
    assert_eq!(row.base_url, None);
    database.finish().await;
}

#[tokio::test]
async fn test_delete_gitea_connection_without_base_url_on_upgraded_database() {
    let (database, db, connection) =
        create_test_database_with_gitea_connection_without_base_url_by_reverting_before_base_url_check_and_running_migrations_up().await;

    crate::rows::delete::<EGitHostConnection, _>(&db, connection.clone())
        .await
        .expect("connection deleted");

    assert!(stored(&db, connection.id).await.is_none());
    database.finish().await;
}

#[tokio::test]
async fn test_gitea_connection_without_base_url_on_upgraded_database_can_be_repaired_by_setting_base_url()
 {
    let (database, db, connection) =
        create_test_database_with_gitea_connection_without_base_url_by_reverting_before_base_url_check_and_running_migrations_up().await;

    write_all_columns_of_git_host_connection(
        &db,
        MGitHostConnection {
            base_url: Some("https://gitea.example.com".into()),
            ..connection.clone()
        },
    )
    .await
    .expect("save base_url");

    let row = stored(&db, connection.id).await.expect("row");
    assert_eq!(row.base_url.as_deref(), Some("https://gitea.example.com"));
    database.finish().await;
}
