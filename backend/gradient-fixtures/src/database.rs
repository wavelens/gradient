/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::time::Duration;

use gradient_migration::{Migrator, MigratorTrait};
use sea_orm::sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, EntityTrait, SqlxPostgresConnector,
    Statement,
};
use uuid::Uuid;

const TEMPLATE: &str = "gradient_template";
const TEMPLATE_DRAFT: &str = "gradient_template_draft";
const TEMPLATE_LOCK: i64 = 7_203_114;
const NO_SERVER: &str = "tests need a Postgres 18 behind PGHOST, PGUSER and PGDATABASE";

pub struct TestDatabase {
    name: String,
    server: PgConnectOptions,
    connection: DatabaseConnection,
    finished: bool,
}

impl TestDatabase {
    pub async fn create() -> Self {
        Self::create_on(PgConnectOptions::new()).await
    }

    pub async fn create_on(server: PgConnectOptions) -> Self {
        let name = format!("test_{}", Uuid::now_v7().simple());
        let admin = connect(server.clone(), 1).await;
        ensure_template(&admin, &server).await;
        run(
            &admin,
            &format!("CREATE DATABASE {name} TEMPLATE {TEMPLATE}"),
        )
        .await;
        close(admin).await;

        Self {
            connection: connect(server.clone().database(&name), 4).await,
            name,
            server,
            finished: false,
        }
    }

    pub fn connection(&self) -> DatabaseConnection {
        self.connection.clone()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub async fn rows<E: EntityTrait>(&self) -> Vec<E::Model> {
        E::find()
            .all(&self.connection)
            .await
            .expect("the table reads")
    }

    pub async fn finish(mut self) {
        self.finished = true;
        let admin = connect(self.server.clone(), 1).await;
        run(&admin, &format!("DROP DATABASE {} WITH (FORCE)", self.name)).await;
        close(admin).await;
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        if std::thread::panicking() {
            eprintln!("test database left for inspection: {}", self.name);
            return;
        }
        panic!("TestDatabase {}: finish() did not run", self.name);
    }
}

async fn connect(options: PgConnectOptions, connections: u32) -> DatabaseConnection {
    let pool = PgPoolOptions::new()
        .max_connections(connections)
        .acquire_timeout(Duration::from_secs(5))
        .connect_with(options)
        .await
        .expect(NO_SERVER);

    SqlxPostgresConnector::from_sqlx_postgres_pool(pool)
}

// The admin pool has a single connection, so the session lock and its release share a session.
async fn ensure_template(admin: &DatabaseConnection, server: &PgConnectOptions) {
    run(admin, &format!("SELECT pg_advisory_lock({TEMPLATE_LOCK})")).await;
    if !exists(admin, TEMPLATE).await {
        run(admin, &format!("DROP DATABASE IF EXISTS {TEMPLATE_DRAFT}")).await;
        run(admin, &format!("CREATE DATABASE {TEMPLATE_DRAFT}")).await;
        let draft = connect(server.clone().database(TEMPLATE_DRAFT), 1).await;
        Migrator::up(&draft, None)
            .await
            .expect("the migrations apply");
        close(draft).await;
        run(
            admin,
            &format!("ALTER DATABASE {TEMPLATE_DRAFT} RENAME TO {TEMPLATE}"),
        )
        .await;
    }
    run(
        admin,
        &format!("SELECT pg_advisory_unlock({TEMPLATE_LOCK})"),
    )
    .await;
}

#[expect(clippy::disallowed_methods, reason = "pg_database is no table of ours")]
async fn exists(db: &DatabaseConnection, database: &str) -> bool {
    db.query_one_raw(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT 1 FROM pg_database WHERE datname = $1",
        [database.into()],
    ))
    .await
    .expect("pg_database reads")
    .is_some()
}

async fn run(db: &DatabaseConnection, sql: &str) {
    db.execute_unprepared(sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

async fn close(db: DatabaseConnection) {
    db.close().await.expect("the connection closes");
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::{cache_role, derivation, user};
    use sea_orm::{EntityTrait, IntoActiveModel};

    #[tokio::test]
    async fn a_test_database_starts_from_the_migrated_template() {
        let db = TestDatabase::create().await;

        assert!(
            !db.rows::<cache_role::Entity>().await.is_empty(),
            "the baseline rows are there"
        );
        assert!(db.rows::<derivation::Entity>().await.is_empty());
        db.finish().await;
    }

    #[tokio::test]
    async fn two_test_databases_share_no_rows() {
        let (first, second) = (TestDatabase::create().await, TestDatabase::create().await);
        user::Entity::insert(crate::rows::user().into_active_model())
            .exec_without_returning(&first.connection())
            .await
            .expect("the user is written");

        assert_eq!(first.rows::<user::Entity>().await.len(), 1);
        assert!(second.rows::<user::Entity>().await.is_empty());
        first.finish().await;
        second.finish().await;
    }

    #[tokio::test]
    async fn databases_asked_for_in_the_same_instant_all_get_the_template() {
        let created = tokio::join!(
            TestDatabase::create(),
            TestDatabase::create(),
            TestDatabase::create(),
            TestDatabase::create()
        );

        for db in [created.0, created.1, created.2, created.3] {
            assert!(
                !db.rows::<cache_role::Entity>().await.is_empty(),
                "{}",
                db.name()
            );
            db.finish().await;
        }
    }

    #[tokio::test]
    async fn a_finished_database_is_gone() {
        let (gone, witness) = (TestDatabase::create().await, TestDatabase::create().await);
        let name = gone.name().to_owned();
        gone.finish().await;

        assert!(!exists(&witness.connection(), &name).await);
        witness.finish().await;
    }

    #[tokio::test]
    #[should_panic(expected = "finish() did not run")]
    async fn a_database_without_finish_fails_its_test() {
        let _db = TestDatabase::create().await;
    }

    #[tokio::test]
    #[should_panic(expected = "the test itself failed")]
    async fn a_failing_test_keeps_its_database_without_a_second_panic() {
        let _db = TestDatabase::create().await;
        panic!("the test itself failed");
    }

    #[tokio::test]
    #[should_panic(expected = "PGHOST")]
    async fn a_server_out_of_contact_names_the_variables() {
        TestDatabase::create_on(PgConnectOptions::new().socket("/nonexistent-postgres-socket"))
            .await;
    }
}
