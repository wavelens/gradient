/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;

use crate::sql_on_one_connection::run_sql_statements_on_one_connection;

const LOCK_TIMEOUT_SQL: &str = "SET lock_timeout = '5s';";

const DROP: &str = "DROP INDEX CONCURRENTLY IF EXISTS \
    public.\"idx-store_path_content_producer-cached_path-kind\";";

const CREATE: &str = "CREATE INDEX CONCURRENTLY \"idx-store_path_content_producer-cached_path-kind\" \
    ON public.store_path_content_producer USING btree (cached_path, kind);";

const RESET_LOCK_TIMEOUT_SQL: &str = "RESET lock_timeout;";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    fn use_transaction(&self) -> Option<bool> {
        Some(false)
    }

    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        run_sql_statements_on_one_connection(
            manager,
            &[LOCK_TIMEOUT_SQL, DROP, CREATE, RESET_LOCK_TIMEOUT_SQL],
        )
        .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        run_sql_statements_on_one_connection(
            manager,
            &[LOCK_TIMEOUT_SQL, DROP, RESET_LOCK_TIMEOUT_SQL],
        )
        .await
    }
}
