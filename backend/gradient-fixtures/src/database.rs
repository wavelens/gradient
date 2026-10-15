/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::time::Duration;

use gradient_entity::evaluation_message::{self, MessageLevel};
use gradient_entity::ids::{DerivationId, EvaluationId, ProjectId, TaskId};
use gradient_entity::{
    build_attempt, build_job, cached_path, cached_path_signature, derivation, derivation_build,
    derivation_dependency, derivation_output, dispatched_job, entry_point, evaluation, task,
};
use gradient_migration::{Migrator, MigratorTrait};
use sea_orm::sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection, EntityTrait,
    PrimaryKeyTrait, QueryFilter, QueryOrder, SqlxPostgresConnector, Statement,
};
use uuid::Uuid;

use crate::shape::build_job_key;

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
            connection: connect(server.clone().database(&name), 16).await,
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

    pub async fn find<E: EntityTrait>(
        &self,
        id: impl Into<<E::PrimaryKey as PrimaryKeyTrait>::ValueType>,
    ) -> Option<E::Model> {
        E::find_by_id(id)
            .one(&self.connection)
            .await
            .expect("the row reads")
    }

    pub async fn execute(&self, sql: &str) {
        run(&self.connection, sql).await;
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "a test reads columns no entity has"
    )]
    pub async fn texts(&self, sql: &str) -> Vec<String> {
        self.connection
            .query_all_raw(Statement::from_string(DatabaseBackend::Postgres, sql))
            .await
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
            .iter()
            .map(|row| row.try_get_by_index(0).expect("a text column"))
            .collect()
    }

    pub async fn find_derivation(&self, path: &str) -> Option<derivation::Model> {
        let (hash, name) = crate::shape::drv_hash_name(path);
        derivation::Entity::find()
            .filter(derivation::Column::Hash.eq(hash))
            .filter(derivation::Column::Name.eq(name))
            .one(&self.connection)
            .await
            .expect("the derivation reads")
    }

    pub async fn derivation(&self, path: &str) -> derivation::Model {
        self.find_derivation(path)
            .await
            .unwrap_or_else(|| panic!("no derivation {path}"))
    }

    pub async fn id_of(&self, path: &str) -> DerivationId {
        self.derivation(path).await.id
    }

    pub async fn shared_build(&self, path: &str) -> derivation_build::Model {
        derivation_build::Entity::find()
            .filter(derivation_build::Column::Derivation.eq(self.id_of(path).await))
            .one(&self.connection)
            .await
            .expect("the shared build reads")
            .unwrap_or_else(|| panic!("no shared build of {path}"))
    }

    pub async fn edge(&self, from: &str, to: &str) -> Option<derivation_dependency::Model> {
        derivation_dependency::Entity::find_by_id((self.id_of(from).await, self.id_of(to).await))
            .one(&self.connection)
            .await
            .expect("the edge reads")
    }

    pub async fn output(&self, hash: &str) -> derivation_output::Model {
        derivation_output::Entity::find()
            .filter(derivation_output::Column::Hash.eq(hash))
            .one(&self.connection)
            .await
            .expect("the output reads")
            .unwrap_or_else(|| panic!("no output {hash}"))
    }

    pub async fn output_of(&self, path: &str, name: &str) -> derivation_output::Model {
        derivation_output::Entity::find()
            .filter(derivation_output::Column::Derivation.eq(self.id_of(path).await))
            .filter(derivation_output::Column::Name.eq(name))
            .one(&self.connection)
            .await
            .expect("the output reads")
            .unwrap_or_else(|| panic!("no output {name} of {path}"))
    }

    pub async fn output_stored(&self, path: &str, name: &str) -> bool {
        let hash = self.output_of(path, name).await.hash;
        self.cached_path(&hash)
            .await
            .is_some_and(|stored| stored.file_hash.is_some())
    }

    pub async fn cached_path(&self, hash: &str) -> Option<cached_path::Model> {
        cached_path::Entity::find()
            .filter(cached_path::Column::Hash.eq(hash))
            .one(&self.connection)
            .await
            .expect("the cached path reads")
    }

    pub async fn signatures(&self, hash: &str) -> Vec<cached_path_signature::Model> {
        let Some(path) = self.cached_path(hash).await else {
            return Vec::new();
        };
        cached_path_signature::Entity::find()
            .filter(cached_path_signature::Column::CachedPath.eq(path.id))
            .all(&self.connection)
            .await
            .expect("the signatures read")
    }

    pub async fn jobs(&self, evaluation: EvaluationId) -> Vec<build_job::Model> {
        build_job::Entity::find()
            .filter(build_job::Column::Evaluation.eq(evaluation))
            .all(&self.connection)
            .await
            .expect("the jobs read")
    }

    pub async fn job(&self, evaluation: EvaluationId, path: &str) -> Option<build_job::Model> {
        let derivation = self.find_derivation(path).await?.id;
        self.jobs(evaluation)
            .await
            .into_iter()
            .find(|job| job.derivation == derivation)
    }

    pub async fn entry_points(&self, evaluation: EvaluationId) -> Vec<entry_point::Model> {
        entry_point::Entity::find()
            .filter(entry_point::Column::Evaluation.eq(evaluation))
            .all(&self.connection)
            .await
            .expect("the entry points read")
    }

    pub async fn entry_point(
        &self,
        evaluation: EvaluationId,
        attr: &str,
    ) -> Option<entry_point::Model> {
        self.entry_points(evaluation)
            .await
            .into_iter()
            .find(|point| point.eval == attr)
    }

    pub async fn attempts(&self, path: &str) -> Vec<build_attempt::Model> {
        build_attempt::Entity::find()
            .filter(build_attempt::Column::DerivationBuild.eq(self.shared_build(path).await.id))
            .order_by_asc(build_attempt::Column::CreatedAt)
            .order_by_asc(build_attempt::Column::Id)
            .all(&self.connection)
            .await
            .expect("the attempts read")
    }

    pub async fn open_assignment(&self, path: &str) -> Option<dispatched_job::Model> {
        dispatched_job::Entity::find()
            .filter(
                dispatched_job::Column::JobId.eq(build_job_key(self.shared_build(path).await.id)),
            )
            .filter(dispatched_job::Column::FinishedAt.is_null())
            .one(&self.connection)
            .await
            .expect("the assignment reads")
    }

    pub async fn error_messages(&self, evaluation: EvaluationId) -> Vec<String> {
        evaluation_message::Entity::find()
            .filter(evaluation_message::Column::Evaluation.eq(evaluation))
            .filter(evaluation_message::Column::Level.eq(MessageLevel::Error))
            .order_by_asc(evaluation_message::Column::CreatedAt)
            .all(&self.connection)
            .await
            .expect("the messages read")
            .into_iter()
            .map(|row| row.message)
            .collect()
    }

    pub async fn task_of(&self, evaluation: EvaluationId) -> TaskId {
        evaluation::Entity::find_by_id(evaluation)
            .one(&self.connection)
            .await
            .expect("the evaluation reads")
            .and_then(|e| e.task)
            .unwrap_or_else(|| panic!("no task of {evaluation}"))
    }

    pub async fn project_of(&self, evaluation: EvaluationId) -> ProjectId {
        task::Entity::find_by_id(self.task_of(evaluation).await)
            .one(&self.connection)
            .await
            .expect("the task reads")
            .expect("the task of the evaluation")
            .project
    }

    pub async fn finish(mut self) {
        self.finished = true;
        let _ = tokio::time::timeout(Duration::from_secs(5), self.connection.clone().close()).await;
        let (admin, name) = (connect(self.server.clone(), 1).await, &self.name);
        run(
            &admin,
            &format!("ALTER DATABASE {name} ALLOW_CONNECTIONS false"),
        )
        .await;
        // `WITH (FORCE)` refuses to end an autovacuum worker without `pg_signal_backend`; a plain drop ends it.
        run(
            &admin,
            &format!(
                "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
                 WHERE datname = '{name}' AND usename = current_user"
            ),
        )
        .await;
        run(&admin, &format!("DROP DATABASE {name}")).await;
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
