/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::Timelike;
use gradient_core::ServerState;
use gradient_graph::{NarCommit, SignTargets};
use gradient_types::ids::{CacheId, ProjectId};
use gradient_types::*;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, EntityTrait, QueryFilter, Statement, Value,
};

pub(super) struct NarUploadRecord<'a> {
    pub file_hash: &'a str,
    pub file_size: i64,
    pub nar_size: i64,
    pub nar_hash: &'a str,
    /// Store-path references in hash-name format (no `/nix/store/` prefix).
    pub references: &'a [String],
    /// Full deriver `.drv` path, if the worker reported one.
    pub deriver: Option<&'a str>,
    /// Content address of the path in narinfo form, if content-addressed.
    pub ca: Option<&'a str>,
}

/// Resolves the project's cache and increments the traffic counter. `project_id` is
/// resolved on the session read loop before the commit detaches, so it stays
/// valid even after the job is evicted from the tracker on completion.
pub(super) async fn record_nar_push_metric(
    state: &ServerState,
    project_id: Option<ProjectId>,
    bytes: i64,
) -> anyhow::Result<()> {
    let Some(project_id) = project_id else {
        return Ok(());
    };

    let project_cache = EProjectCache::find()
        .filter(CProjectCache::Project.eq(project_id))
        .one(&state.worker_db)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no cache for project {}", project_id))?;

    let cache_id = project_cache.cache;
    let now = gradient_types::now();
    let bucket = now
        .with_second(0)
        .and_then(|t: chrono::NaiveDateTime| t.with_nanosecond(0))
        .unwrap_or(now);

    upsert_cache_metric(state, cache_id, bucket, bytes).await
}

async fn upsert_cache_metric(
    state: &ServerState,
    cache_id: CacheId,
    bucket: chrono::NaiveDateTime,
    bytes: i64,
) -> anyhow::Result<()> {
    // Atomic accumulate keyed on the (cache, bucket_time) unique index: concurrent
    // NAR commits for the same cache in one minute otherwise race a find-then-insert
    // into a duplicate-key violation (and the update arm loses each other's writes).
    state
        .worker_db
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO cache_metric (id, cache, bucket_time, bytes_sent, nar_count) \
             VALUES (uuidv7(), $1, $2, $3, 1) \
             ON CONFLICT (cache, bucket_time) DO UPDATE SET \
                 bytes_sent = cache_metric.bytes_sent + EXCLUDED.bytes_sent, \
                 nar_count  = cache_metric.nar_count  + 1",
            [
                Value::Uuid(Some(cache_id.into_inner())),
                bucket.into(),
                bytes.into(),
            ],
        ))
        .await?;

    Ok(())
}

pub(super) async fn mark_nar_stored(
    state: &ServerState,
    project_id: Option<ProjectId>,
    store_path: &str,
    record: &NarUploadRecord<'_>,
) -> anyhow::Result<()> {
    let hash_name = store_path.strip_prefix("/nix/store/").unwrap_or(store_path);
    let hash = hash_name.split('-').next().unwrap_or("");

    if hash.is_empty() {
        return Ok(());
    }

    let targets = match project_id {
        Some(project_id) => SignTargets::ProjectCaches(project_id),
        None => SignTargets::None,
    };
    let committed = state
        .graph
        .commit_nar(NarCommit {
            store_path: store_path.to_owned(),
            file_hash: record.file_hash.to_owned(),
            file_size: record.file_size,
            nar_size: record.nar_size,
            nar_hash: record.nar_hash.to_owned(),
            references: record.references.to_vec(),
            deriver: record.deriver.map(str::to_owned),
            ca: record.ca.map(str::to_owned),
            targets,
        })
        .await?;

    // Sign this specific path in place so its narinfo is servable immediately,
    // rather than waking a whole-table sweep. Placeholder rows only exist when a
    // cache took it (ProjectCaches); the periodic sweep stays the backfill.
    if project_id.is_some() {
        crate::signing::sign_cached_path(
            &state.worker_db,
            &state.config.secrets.crypt_secret_file,
            &state.config.server.serve_url,
            crate::signing::SignRequest {
                cached_path: committed.cached_path,
                store_path,
                nar_hash: record.nar_hash,
                nar_size: record.nar_size,
                references: record.references,
            },
        )
        .await;
    }

    Ok(())
}
