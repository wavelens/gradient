/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;

use crate::sql_on_one_connection::run_sql_statements_on_one_connection;

const LOCK_TIMEOUT_SQL: &str = "SET lock_timeout = '5s';";

const DROP_PROJECT_INDEX: &str =
    "DROP INDEX CONCURRENTLY IF EXISTS public.\"idx-eval_cache_store-cache-project-fingerprint\";";

const CREATE_PROJECT_INDEX: &str = "CREATE UNIQUE INDEX CONCURRENTLY \
    \"idx-eval_cache_store-cache-project-fingerprint\" \
    ON public.eval_cache_store (cache, project, fingerprint);";

const DROP_FINGERPRINT_INDEX: &str =
    "DROP INDEX CONCURRENTLY IF EXISTS public.\"idx-eval_cache_store-cache-fingerprint\";";

const CREATE_FINGERPRINT_INDEX: &str = "CREATE UNIQUE INDEX CONCURRENTLY \
    \"idx-eval_cache_store-cache-fingerprint\" \
    ON public.eval_cache_store (cache, fingerprint);";

const DELETE_ROWS_WITHOUT_PROJECT: &str =
    "DELETE FROM public.eval_cache_store WHERE project IS NULL;";

// Rows are cache entries. The down migration can keep the most recently updated row for each cache and fingerprint so it can rebuild the old unique index.
const DELETE_ROWS_THE_FINGERPRINT_INDEX_REFUSES: &str = "DELETE FROM public.eval_cache_store AS older \
    USING public.eval_cache_store AS newer \
    WHERE older.cache = newer.cache \
    AND older.fingerprint = newer.fingerprint \
    AND (older.updated_at, older.id) < (newer.updated_at, newer.id);";

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
                DROP_PROJECT_INDEX,
                CREATE_PROJECT_INDEX,
                DROP_FINGERPRINT_INDEX,
                DELETE_ROWS_WITHOUT_PROJECT,
                RESET_LOCK_TIMEOUT_SQL,
            ],
        )
        .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        run_sql_statements_on_one_connection(
            manager,
            &[
                LOCK_TIMEOUT_SQL,
                DROP_FINGERPRINT_INDEX,
                DELETE_ROWS_THE_FINGERPRINT_INDEX_REFUSES,
                CREATE_FINGERPRINT_INDEX,
                DROP_PROJECT_INDEX,
                RESET_LOCK_TIMEOUT_SQL,
            ],
        )
        .await
    }
}
