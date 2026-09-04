/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Typed wrappers around the `SeaORM` connection pools.
//!
//! Gradient runs separate pools so HTTP requests served by the axum layer cannot
//! be starved by the proto/scheduler/cache work. [`WebDb`], [`WorkerDb`] and
//! [`CacheDb`] are newtypes that forward `ConnectionTrait`, so call sites
//! (`find().one(&ctx.web_db)`, ...) work unchanged while the newtypes stay
//! non-substitutable at any explicitly typed boundary. [`WorkerDb`] can also
//! stand for one open transaction on its pool, which is how the graph actor runs
//! every `gradient_db` function inside one transaction.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use sea_orm::{
    AccessMode, ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, DbErr,
    ExecResult, IsolationLevel, QueryResult, Statement, TransactionError, TransactionOptions,
    TransactionTrait,
};

/// The pool dedicated to axum/HTTP request handling. Use this from any
/// `gradient_web::endpoints::*` handler so HTTP latency is not coupled to the
/// proto/scheduler workload. `Arc`-wrapped so the context slices that carry it
/// stay cheaply `Clone` (`DatabaseConnection` itself is not `Clone`).
#[derive(Debug, Clone)]
pub struct WebDb(Arc<DatabaseConnection>);

/// The pool used by the proto handler, scheduler, cache GC, and any tracked
/// background task; or, inside the graph actor, one open transaction on it.
#[derive(Debug, Clone)]
pub struct WorkerDb(WorkerConn);

#[derive(Debug, Clone)]
enum WorkerConn {
    Pool(Arc<DatabaseConnection>),
    Transaction {
        tx: Arc<DatabaseTransaction>,
        pool: Arc<DatabaseConnection>,
    },
}

/// The pool dedicated to the cache-query read path (`CacheQuery` prefetch
/// lookups). Isolated from [`WorkerDb`] so a large eval's prefetch storm cannot
/// exhaust the scheduler/dispatch pool - a saturated cache pool then only slows
/// cache queries (which degrade to a retryable [`crate`] miss) instead of
/// stalling the whole scheduler.
#[derive(Debug, Clone)]
pub struct CacheDb(Arc<DatabaseConnection>);

impl WebDb {
    pub fn new(conn: DatabaseConnection) -> Self {
        Self(Arc::new(conn))
    }

    /// Borrow the inner `DatabaseConnection` - needed in the few places
    /// where a function signature is hard-coded to `&DatabaseConnection`
    /// instead of `&impl ConnectionTrait`.
    pub fn inner(&self) -> &DatabaseConnection {
        self.0.as_ref()
    }
}

impl WorkerDb {
    pub fn new(conn: DatabaseConnection) -> Self {
        Self(WorkerConn::Pool(Arc::new(conn)))
    }

    /// A handle whose every statement runs on `tx`; `detached` gives the pool back.
    pub fn in_transaction(&self, tx: Arc<DatabaseTransaction>) -> Self {
        Self(WorkerConn::Transaction {
            tx,
            pool: Arc::clone(self.pool()),
        })
    }

    /// The pool, for work that must outlive or sit outside the current transaction.
    pub fn detached(&self) -> Self {
        Self(WorkerConn::Pool(Arc::clone(self.pool())))
    }

    pub fn is_transactional(&self) -> bool {
        matches!(self.0, WorkerConn::Transaction { .. })
    }

    fn pool(&self) -> &Arc<DatabaseConnection> {
        match &self.0 {
            WorkerConn::Pool(pool) | WorkerConn::Transaction { pool, .. } => pool,
        }
    }

    #[cfg(test)]
    pub fn into_transaction_log(self) -> Vec<sea_orm::Transaction> {
        match self.0 {
            WorkerConn::Pool(pool) => Arc::try_unwrap(pool)
                .expect("the pool handle is unique in tests")
                .into_transaction_log(),
            WorkerConn::Transaction { .. } => panic!("not a pool"),
        }
    }
}

impl CacheDb {
    pub fn new(conn: DatabaseConnection) -> Self {
        Self(Arc::new(conn))
    }

    pub fn inner(&self) -> &DatabaseConnection {
        self.0.as_ref()
    }
}

macro_rules! impl_connection_trait {
    ($ty:ty) => {
        #[async_trait::async_trait]
        impl ConnectionTrait for $ty {
            fn get_database_backend(&self) -> DbBackend {
                self.0.get_database_backend()
            }

            async fn execute_raw(&self, stmt: Statement) -> Result<ExecResult, DbErr> {
                self.0.execute_raw(stmt).await
            }

            async fn execute_unprepared(&self, sql: &str) -> Result<ExecResult, DbErr> {
                self.0.execute_unprepared(sql).await
            }

            async fn query_one_raw(&self, stmt: Statement) -> Result<Option<QueryResult>, DbErr> {
                self.0.query_one_raw(stmt).await
            }

            async fn query_all_raw(&self, stmt: Statement) -> Result<Vec<QueryResult>, DbErr> {
                self.0.query_all_raw(stmt).await
            }

            fn support_returning(&self) -> bool {
                self.0.support_returning()
            }

            fn is_mock_connection(&self) -> bool {
                self.0.is_mock_connection()
            }
        }
    };
}

impl_connection_trait!(WebDb);
impl_connection_trait!(CacheDb);

#[async_trait::async_trait]
impl ConnectionTrait for WorkerDb {
    fn get_database_backend(&self) -> DbBackend {
        match &self.0 {
            WorkerConn::Pool(pool) => pool.get_database_backend(),
            WorkerConn::Transaction { tx, .. } => tx.get_database_backend(),
        }
    }

    async fn execute_raw(&self, stmt: Statement) -> Result<ExecResult, DbErr> {
        match &self.0 {
            WorkerConn::Pool(pool) => pool.execute_raw(stmt).await,
            WorkerConn::Transaction { tx, .. } => tx.execute_raw(stmt).await,
        }
    }

    async fn execute_unprepared(&self, sql: &str) -> Result<ExecResult, DbErr> {
        match &self.0 {
            WorkerConn::Pool(pool) => pool.execute_unprepared(sql).await,
            WorkerConn::Transaction { tx, .. } => tx.execute_unprepared(sql).await,
        }
    }

    async fn query_one_raw(&self, stmt: Statement) -> Result<Option<QueryResult>, DbErr> {
        match &self.0 {
            WorkerConn::Pool(pool) => pool.query_one_raw(stmt).await,
            WorkerConn::Transaction { tx, .. } => tx.query_one_raw(stmt).await,
        }
    }

    async fn query_all_raw(&self, stmt: Statement) -> Result<Vec<QueryResult>, DbErr> {
        match &self.0 {
            WorkerConn::Pool(pool) => pool.query_all_raw(stmt).await,
            WorkerConn::Transaction { tx, .. } => tx.query_all_raw(stmt).await,
        }
    }

    fn support_returning(&self) -> bool {
        self.pool().support_returning()
    }

    fn is_mock_connection(&self) -> bool {
        self.pool().is_mock_connection()
    }
}

#[async_trait::async_trait]
impl TransactionTrait for WorkerDb {
    type Transaction = DatabaseTransaction;

    async fn begin(&self) -> Result<DatabaseTransaction, DbErr> {
        match &self.0 {
            WorkerConn::Pool(pool) => pool.begin().await,
            WorkerConn::Transaction { tx, .. } => tx.begin().await,
        }
    }

    async fn begin_with_config(
        &self,
        isolation_level: Option<IsolationLevel>,
        access_mode: Option<AccessMode>,
    ) -> Result<DatabaseTransaction, DbErr> {
        match &self.0 {
            WorkerConn::Pool(pool) => pool.begin_with_config(isolation_level, access_mode).await,
            WorkerConn::Transaction { tx, .. } => {
                tx.begin_with_config(isolation_level, access_mode).await
            }
        }
    }

    async fn begin_with_options(
        &self,
        options: TransactionOptions,
    ) -> Result<DatabaseTransaction, DbErr> {
        match &self.0 {
            WorkerConn::Pool(pool) => pool.begin_with_options(options).await,
            WorkerConn::Transaction { tx, .. } => tx.begin_with_options(options).await,
        }
    }

    async fn transaction<F, T, E>(&self, callback: F) -> Result<T, TransactionError<E>>
    where
        F: for<'c> FnOnce(
                &'c DatabaseTransaction,
            ) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'c>>
            + Send,
        T: Send,
        E: std::fmt::Display + std::fmt::Debug + Send,
    {
        match &self.0 {
            WorkerConn::Pool(pool) => pool.transaction(callback).await,
            WorkerConn::Transaction { tx, .. } => tx.transaction(callback).await,
        }
    }

    async fn transaction_with_config<F, T, E>(
        &self,
        callback: F,
        isolation_level: Option<IsolationLevel>,
        access_mode: Option<AccessMode>,
    ) -> Result<T, TransactionError<E>>
    where
        F: for<'c> FnOnce(
                &'c DatabaseTransaction,
            ) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'c>>
            + Send,
        T: Send,
        E: std::fmt::Display + std::fmt::Debug + Send,
    {
        match &self.0 {
            WorkerConn::Pool(pool) => {
                pool.transaction_with_config(callback, isolation_level, access_mode)
                    .await
            }
            WorkerConn::Transaction { tx, .. } => {
                tx.transaction_with_config(callback, isolation_level, access_mode)
                    .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{MockDatabase, MockExecResult};

    /// Regression for #68: a function typed `fn(&WebDb)` must not accept a
    /// `&WorkerDb` (and vice versa). The two newtypes are non-substitutable
    /// at any explicitly-typed function boundary, which is the compile-time
    /// defense the issue asked for.
    #[test]
    fn newtypes_are_non_substitutable() {
        fn takes_web(_: &WebDb) {}
        fn takes_worker(_: &WorkerDb) {}

        let web = WebDb::new(MockDatabase::new(DbBackend::Postgres).into_connection());
        let worker = WorkerDb::new(MockDatabase::new(DbBackend::Postgres).into_connection());

        takes_web(&web);
        takes_worker(&worker);

        // The following lines, if uncommented, must fail to compile:
        // takes_web(&worker);
        // takes_worker(&web);
    }

    /// `&WebDb` / `&WorkerDb` satisfy `&impl ConnectionTrait`, so existing
    /// SeaORM call sites keep working without `.inner()` boilerplate.
    #[tokio::test]
    async fn forwards_connection_trait() {
        async fn run<C: ConnectionTrait>(db: &C) -> DbBackend {
            db.get_database_backend()
        }
        let web = WebDb::new(MockDatabase::new(DbBackend::Postgres).into_connection());
        let worker = WorkerDb::new(MockDatabase::new(DbBackend::Postgres).into_connection());
        assert_eq!(run(&web).await, DbBackend::Postgres);
        assert_eq!(run(&worker).await, DbBackend::Postgres);
    }

    #[tokio::test]
    async fn a_transactional_handle_runs_inside_the_transaction_and_detaches_to_the_pool() {
        let mock = MockDatabase::new(DbBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let pool = WorkerDb::new(mock);
        let tx = Arc::new(pool.begin().await.expect("begin"));
        let scoped = pool.in_transaction(Arc::clone(&tx));
        assert!(scoped.is_transactional());
        assert!(!scoped.detached().is_transactional());

        scoped
            .execute_raw(Statement::from_string(
                DbBackend::Postgres,
                "UPDATE derivation_build SET status = 1",
            ))
            .await
            .expect("statement on the transaction");
        drop(scoped);

        let tx = Arc::try_unwrap(tx).expect("no handle outlives the scope");
        tx.commit().await.expect("commit");

        let log = pool.into_transaction_log();
        assert_eq!(log.len(), 1, "one transaction: {log:?}");
    }

    #[tokio::test]
    async fn begin_on_a_transactional_handle_is_a_savepoint() {
        let pool = WorkerDb::new(MockDatabase::new(DbBackend::Postgres).into_connection());
        let outer = Arc::new(pool.begin().await.expect("begin"));
        let scoped = pool.in_transaction(Arc::clone(&outer));
        let inner = scoped.begin().await.expect("savepoint");
        inner.rollback().await.expect("rollback to savepoint");
        drop(scoped);
        Arc::try_unwrap(outer)
            .expect("unique")
            .commit()
            .await
            .expect("commit");
    }
}
