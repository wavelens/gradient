/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use sea_orm::DatabaseConnection;
use sea_orm::sqlx::{AssertSqlSafe, PgConnection, query_scalar};
use std::time::Duration;
use tokio::time::Instant;

const SCHEMA_PREPARATION_LOCK: i64 = 646;
const SCHEMA_PREPARATION_LOCK_RETRY_DELAY: Duration = Duration::from_secs(1);
const SCHEMA_PREPARATION_LOCK_WAIT_LOG_INTERVAL: Duration = Duration::from_secs(30);

crate::sql! {
    SCHEMA_PREPARATION_LOCK_TRY = "SELECT pg_try_advisory_lock($1) AS locked",
        params = [Int(SCHEMA_PREPARATION_LOCK)];
}

// The connection can never return to the pool. Connection drops close the session. All exit paths, including panic or crash, leave the lock released by Postgres.
pub(super) struct SchemaPreparationAdvisoryLockConnection {
    _connection: PgConnection,
}

pub(super) async fn take_schema_preparation_lock(
    db: &DatabaseConnection,
) -> Result<SchemaPreparationAdvisoryLockConnection> {
    let mut connection = db
        .get_postgres_connection_pool()
        .acquire()
        .await
        .context("open a dedicated database connection for locking during schema preparation")?
        .detach();
    let mut next_wait_log = Instant::now();
    while !try_acquire_schema_preparation_lock(&mut connection).await? {
        if Instant::now() >= next_wait_log {
            tracing::info!("waiting for another instance to prepare the database schema");
            next_wait_log = Instant::now() + SCHEMA_PREPARATION_LOCK_WAIT_LOG_INTERVAL;
        }
        tokio::time::sleep(SCHEMA_PREPARATION_LOCK_RETRY_DELAY).await;
    }

    Ok(SchemaPreparationAdvisoryLockConnection {
        _connection: connection,
    })
}

async fn try_acquire_schema_preparation_lock(connection: &mut PgConnection) -> Result<bool> {
    query_scalar(AssertSqlSafe(SCHEMA_PREPARATION_LOCK_TRY.text()))
        .bind(SCHEMA_PREPARATION_LOCK)
        .fetch_one(connection)
        .await
        .context("try to acquire the lock for schema preparation")
}

#[cfg(test)]
mod tests {
    use super::super::bring_database_schema_up_to_date;
    use super::take_schema_preparation_lock;
    use gradient_fixtures::database::TestDatabase;
    use gradient_migration::Migrator;
    use sea_orm_migration::MigratorTrait;
    use std::time::Duration;

    #[tokio::test]
    async fn concurrent_schema_preparation_applies_each_pending_migration_once_and_both_instances_succeed()
     {
        let db = TestDatabase::create().await;
        let connection = db.connection();
        Migrator::down(&connection, Some(1))
            .await
            .expect("revert the latest migration");

        let (first, second) = tokio::join!(
            bring_database_schema_up_to_date(&connection),
            bring_database_schema_up_to_date(&connection),
        );
        first.expect("first instance prepares schema");
        second.expect("schema preparation by second instance");

        let migrations = Migrator::migrations().len();
        let counts = db
            .texts(
                "SELECT format('%s/%s', count(*), count(DISTINCT version)) FROM seaql_migrations",
            )
            .await;
        assert_eq!(counts, [format!("{migrations}/{migrations}")]);
        db.finish().await;
    }

    #[tokio::test]
    async fn starting_instance_waits_for_schema_preparation_lock() {
        let db = TestDatabase::create().await;
        let connection = db.connection();
        Migrator::down(&connection, Some(1))
            .await
            .expect("revert the latest migration");
        let migrations = Migrator::migrations().len();
        let lock = take_schema_preparation_lock(&connection)
            .await
            .expect("test: acquire lock for schema preparation");

        let preparation = bring_database_schema_up_to_date(&connection);
        tokio::pin!(preparation);
        assert!(
            tokio::time::timeout(Duration::from_millis(300), &mut preparation)
                .await
                .is_err(),
            "instance can wait on a lock held by another process"
        );
        assert_eq!(
            db.texts("SELECT count(*)::text FROM seaql_migrations")
                .await,
            [(migrations - 1).to_string()],
            "migration pending, lock held by another instance"
        );

        drop(lock);
        preparation
            .await
            .expect("instance can prepare schema after lock release");
        db.finish().await;
    }
}
