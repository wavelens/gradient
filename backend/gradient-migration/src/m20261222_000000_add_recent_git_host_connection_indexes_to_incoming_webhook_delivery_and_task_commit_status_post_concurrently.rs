/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;

use crate::sql_on_one_connection::run_sql_statements_on_one_connection;

const LOCK_TIMEOUT_SQL: &str = "SET lock_timeout = '5s';";

const DROP_INDEX_STATEMENTS: [&str; 2] = [
    "DROP INDEX CONCURRENTLY IF EXISTS public.task_commit_status_post_attempt_connection_delivered_at;",
    "DROP INDEX CONCURRENTLY IF EXISTS public.incoming_webhook_delivery_connection_repository_created_at;",
];

const CREATE_CONCURRENT_INDEXES_SQL: [&str; 2] = [
    "CREATE INDEX CONCURRENTLY task_commit_status_post_attempt_connection_delivered_at \
        ON public.task_commit_status_post_attempt (git_host_connection, delivered_at DESC);",
    "CREATE INDEX CONCURRENTLY incoming_webhook_delivery_connection_repository_created_at \
        ON public.incoming_webhook_delivery (git_host_connection, repository, created_at DESC);",
];

const RESET_LOCK_TIMEOUT_SQL: &str = "RESET lock_timeout;";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    fn use_transaction(&self) -> Option<bool> {
        Some(false)
    }

    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let statements: Vec<&str> = [LOCK_TIMEOUT_SQL]
            .into_iter()
            .chain(DROP_INDEX_STATEMENTS)
            .chain(CREATE_CONCURRENT_INDEXES_SQL)
            .chain([RESET_LOCK_TIMEOUT_SQL])
            .collect();
        run_sql_statements_on_one_connection(manager, &statements).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let statements: Vec<&str> = [LOCK_TIMEOUT_SQL]
            .into_iter()
            .chain(DROP_INDEX_STATEMENTS)
            .chain([RESET_LOCK_TIMEOUT_SQL])
            .collect();
        run_sql_statements_on_one_connection(manager, &statements).await
    }
}
