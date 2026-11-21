/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;

use crate::sql_on_one_connection::run_sql_statements_on_one_connection;

const LOCK_TIMEOUT_SQL: &str = "SET lock_timeout = '5s';";

const DROP_NAR_OBJECT_STORAGE_KEY_INDEX: &str =
    "DROP INDEX CONCURRENTLY IF EXISTS public.\"idx-cached_path-nar_object_storage_key\";";

const CREATE_NAR_OBJECT_STORAGE_KEY_INDEX: &str = "CREATE INDEX CONCURRENTLY \
    \"idx-cached_path-nar_object_storage_key\" ON public.cached_path (nar_object_storage_key);";

const DROP_SERVER_COMPUTED_CONTENT_INDEX: &str = "DROP INDEX CONCURRENTLY IF EXISTS \
    public.\"uq-cached_path-server_computed_hash_nar_hash_references_ca\";";

const CREATE_SERVER_COMPUTED_CONTENT_INDEX: &str = "CREATE UNIQUE INDEX CONCURRENTLY \
    \"uq-cached_path-server_computed_hash_nar_hash_references_ca\" \
    ON public.cached_path (hash, nar_hash, md5(coalesce(\"references\", '') || E'\\n' || coalesce(ca, ''))) \
    WHERE server_computed_hashes_and_sizes;";

const DROP_SIGNATURE_INDEX: &str = "DROP INDEX CONCURRENTLY IF EXISTS \
    public.\"idx-cached_path_signature-cache-store_path_hash\";";

const DELETE_NEWER_DUPLICATE_SIGNATURES: &str = "DELETE FROM public.cached_path_signature newer \
    WHERE EXISTS (SELECT 1 FROM public.cached_path_signature older \
    WHERE older.cache = newer.cache AND older.store_path_hash = newer.store_path_hash \
    AND (coalesce(older.signature, older.previous_signing_key_signature) IS NULL, older.created_at, older.id) \
    < (coalesce(newer.signature, newer.previous_signing_key_signature) IS NULL, newer.created_at, newer.id));";

const CREATE_SIGNATURE_INDEX: &str = "CREATE UNIQUE INDEX CONCURRENTLY \
    \"idx-cached_path_signature-cache-store_path_hash\" \
    ON public.cached_path_signature (cache, store_path_hash);";

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
                DROP_NAR_OBJECT_STORAGE_KEY_INDEX,
                CREATE_NAR_OBJECT_STORAGE_KEY_INDEX,
                DROP_SERVER_COMPUTED_CONTENT_INDEX,
                CREATE_SERVER_COMPUTED_CONTENT_INDEX,
                DROP_SIGNATURE_INDEX,
                DELETE_NEWER_DUPLICATE_SIGNATURES,
                CREATE_SIGNATURE_INDEX,
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
                DROP_SIGNATURE_INDEX,
                DROP_SERVER_COMPUTED_CONTENT_INDEX,
                DROP_NAR_OBJECT_STORAGE_KEY_INDEX,
                RESET_LOCK_TIMEOUT_SQL,
            ],
        )
        .await
    }
}
