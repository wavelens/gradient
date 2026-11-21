/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use sea_orm::sea_query::OnConflict;
use sea_orm::{ConnectionTrait, EntityTrait, Set, Value};

use gradient_types::*;

pub use gradient_migration::add_project_to_eval_cache_store_migration_version;

crate::sql! {
    SELECT_APPLIED_AT_FROM_SEAQL_MIGRATIONS_BY_VERSION =
        "SELECT applied_at FROM seaql_migrations WHERE version = $1",
        params = [Text("m20261230_000000_add_project_to_eval_cache_store")];

    SELECT_STORAGE_MIGRATION_BY_NAME_WITH_APPLIED_AT_NOT_NULL =
        "SELECT 1 AS applied FROM storage_migration WHERE name = $1 AND applied_at IS NOT NULL",
        params = [Text("m20261230_000000_delete_eval_cache_segments_modified_before_project_column")];
}

pub const DELETE_EVALUATION_CACHE_SEGMENTS_WRITTEN_BEFORE_UPGRADE_MIGRATION_NAME: &str =
    "m20261230_000000_delete_eval_cache_segments_modified_before_project_column";

pub async fn is_storage_migration_applied<C: ConnectionTrait>(
    conn: &C,
    name: &str,
) -> Result<bool> {
    Ok(conn
        .query_one_raw(
            SELECT_STORAGE_MIGRATION_BY_NAME_WITH_APPLIED_AT_NOT_NULL
                .bind([Value::String(Some(name.to_owned()))]),
        )
        .await
        .context("read storage_migration applied_at")?
        .is_some())
}

pub async fn migration_applied_at_unix_seconds<C: ConnectionTrait>(
    conn: &C,
    version: &str,
) -> Result<Option<i64>> {
    conn.query_one_raw(
        SELECT_APPLIED_AT_FROM_SEAQL_MIGRATIONS_BY_VERSION
            .bind([Value::String(Some(version.to_owned()))]),
    )
    .await
    .context("read seaql_migrations applied_at")?
    .map(|row| row.try_get::<i64>("", "applied_at"))
    .transpose()
    .context("read seaql_migrations applied_at")
}

pub async fn all<C: ConnectionTrait>(conn: &C) -> Result<Vec<MStorageMigration>> {
    EStorageMigration::find()
        .all(conn)
        .await
        .context("list storage_migration")
}

pub async fn save_checkpoint<C: ConnectionTrait>(
    conn: &C,
    name: &str,
    checkpoint: &str,
) -> Result<()> {
    EStorageMigration::insert(AStorageMigration {
        name: Set(name.to_owned()),
        checkpoint: Set(Some(checkpoint.to_owned())),
        started_at: Set(now()),
        applied_at: Set(None),
    })
    .on_conflict(
        OnConflict::column(CStorageMigration::Name)
            .update_column(CStorageMigration::Checkpoint)
            .to_owned(),
    )
    .exec_without_returning(conn)
    .await
    .context("save storage_migration checkpoint")?;
    Ok(())
}

pub async fn mark_applied<C: ConnectionTrait>(conn: &C, name: &str) -> Result<()> {
    EStorageMigration::insert(AStorageMigration {
        name: Set(name.to_owned()),
        checkpoint: Set(None),
        started_at: Set(now()),
        applied_at: Set(Some(now())),
    })
    .on_conflict(
        OnConflict::column(CStorageMigration::Name)
            .update_columns([CStorageMigration::Checkpoint, CStorageMigration::AppliedAt])
            .to_owned(),
    )
    .exec_without_returning(conn)
    .await
    .context("mark storage_migration applied")?;
    Ok(())
}
