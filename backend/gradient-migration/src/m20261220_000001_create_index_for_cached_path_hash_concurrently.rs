/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;

use crate::sql_on_one_connection::run_sql_statements_on_one_connection;

const LOCK_TIMEOUT_SQL: &str = "SET lock_timeout = '5s';";

const DROP_HASH_INDEX: &str = "DROP INDEX CONCURRENTLY IF EXISTS public.\"idx-cached_path-hash\";";

const CREATE_HASH_INDEX: &str =
    "CREATE INDEX CONCURRENTLY \"idx-cached_path-hash\" ON public.cached_path (hash);";

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
            &[
                LOCK_TIMEOUT_SQL,
                DROP_HASH_INDEX,
                CREATE_HASH_INDEX,
                RESET_LOCK_TIMEOUT_SQL,
            ],
        )
        .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        run_sql_statements_on_one_connection(
            manager,
            &[LOCK_TIMEOUT_SQL, DROP_HASH_INDEX, RESET_LOCK_TIMEOUT_SQL],
        )
        .await
    }
}
