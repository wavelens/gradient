/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Typed wrappers around the two `SeaORM` connection pools.
//!
//! Gradient runs two pools so HTTP requests served by the axum layer cannot
//! be starved by the proto/scheduler/cache work. [`WebDb`] and [`WorkerDb`]
//! are newtypes around `DatabaseConnection` that both forward
//! `ConnectionTrait`, so existing call sites (`find().one(&ctx.web_db)`, …)
//! work unchanged while the two newtypes stay non-substitutable at any
//! explicitly-typed `&WebDb` / `&WorkerDb` boundary.

use std::sync::Arc;

use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, DbErr, ExecResult, QueryResult, Statement,
};

/// The pool dedicated to axum/HTTP request handling. Use this from any
/// `gradient_web::endpoints::*` handler so HTTP latency is not coupled to the
/// proto/scheduler workload. `Arc`-wrapped so the context slices that carry it
/// stay cheaply `Clone` (`DatabaseConnection` itself is not `Clone`).
#[derive(Debug, Clone)]
pub struct WebDb(Arc<DatabaseConnection>);

/// The pool used by the proto handler, scheduler, cache GC, and any
/// fire-and-forget background task spawned from a web handler that should
/// not contend with foreground HTTP requests.
#[derive(Debug, Clone)]
pub struct WorkerDb(Arc<DatabaseConnection>);

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

            async fn execute(&self, stmt: Statement) -> Result<ExecResult, DbErr> {
                self.0.execute(stmt).await
            }

            async fn execute_unprepared(&self, sql: &str) -> Result<ExecResult, DbErr> {
                self.0.execute_unprepared(sql).await
            }

            async fn query_one(&self, stmt: Statement) -> Result<Option<QueryResult>, DbErr> {
                self.0.query_one(stmt).await
            }

            async fn query_all(&self, stmt: Statement) -> Result<Vec<QueryResult>, DbErr> {
                self.0.query_all(stmt).await
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
impl_connection_trait!(WorkerDb);

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::MockDatabase;

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
}
