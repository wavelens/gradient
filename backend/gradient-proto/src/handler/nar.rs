/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::Timelike;
use gradient_core::ServerState;
use gradient_entity::dispatched_job::{Column as CDispatchedJob, Entity as EDispatchedJob};
use gradient_graph::{NarCommit, SignTargets};
use gradient_types::ids::{CacheId, ProjectId};
use gradient_types::*;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, EntityTrait, QueryFilter, QueryOrder, Select,
    Statement, Value,
};
use tracing::warn;

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

/// The dispatch row binding `job_id` to the project it was dispatched for.
fn dispatched_job_query(worker_id: &str, job_id: &str) -> Select<EDispatchedJob> {
    EDispatchedJob::find()
        .filter(CDispatchedJob::JobId.eq(job_id))
        .filter(CDispatchedJob::WorkerId.eq(worker_id))
        .order_by_desc(CDispatchedJob::DispatchedAt)
}

/// The project a job was dispatched for, read from the durable dispatch row.
///
/// `JobCompleted` rides the control writer lane while `NarUploaded` rides bulk,
/// and `WriterLanes` always drains control first - so a job's completion
/// overtakes its own trailing NAR confirmations and the scheduler has already
/// evicted the job by the time a late NAR commits. Without this fallback the
/// commit takes `SignTargets::None`, writing a `cached_path` row that no cache
/// claims: the narinfo gate 404s it forever, and the sign sweep cannot repair it
/// because it only fills rows that already exist.
///
/// Keyed on the worker as well as the job so a concurrent dispatch of the same
/// job key elsewhere cannot answer for this connection's upload.
pub(super) async fn project_for_dispatched_job<C: ConnectionTrait>(
    db: &C,
    worker_id: &str,
    job_id: &str,
) -> Option<ProjectId> {
    dispatched_job_query(worker_id, job_id)
        .one(db)
        .await
        .map_err(|e| warn!(%job_id, %worker_id, error = %e, "dispatched_job lookup for a late NAR failed"))
        .ok()
        .flatten()
        .map(|row| row.project)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{MockDatabase, QueryTrait};
    use uuid::Uuid;

    const JOB: &str = "build:01a07af6-9d9f-7300-8aa1-fb82b0c18446";
    const WORKER: &str = "builder-1";

    fn project() -> ProjectId {
        ProjectId::new(
            Uuid::parse_str("019e8549-eef3-7b92-96a2-f3842f585407").expect("project uuid"),
        )
    }

    fn row() -> gradient_entity::dispatched_job::Model {
        gradient_entity::dispatched_job::Model {
            id: gradient_entity::ids::DispatchedJobId::now_v7(),
            project: project(),
            worker_id: WORKER.to_owned(),
            job_id: Some(JOB.to_owned()),
            ..Default::default()
        }
    }

    /// The whole point of the fallback: the scheduler has already evicted the
    /// job, and the dispatch row is what keeps the NAR's cache claim.
    #[tokio::test]
    async fn a_late_nar_still_resolves_its_project_from_the_dispatch_row() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row()]])
            .into_connection();

        assert_eq!(
            project_for_dispatched_job(&db, WORKER, JOB).await,
            Some(project())
        );
    }

    /// No dispatch row is the one case that legitimately has no project. It must
    /// return `None` rather than panic, so the caller can log and carry on.
    #[tokio::test]
    async fn no_dispatch_row_yields_no_project() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<gradient_entity::dispatched_job::Model>::new()])
            .into_connection();

        assert_eq!(project_for_dispatched_job(&db, WORKER, JOB).await, None);
    }

    /// A job key is unique only among in-flight jobs, so the same `build:<anchor>`
    /// recurs across evaluations - and those may belong to different projects.
    /// Without the worker filter and the newest-first order, a late NAR could be
    /// signed into a stranger's caches.
    #[test]
    fn the_lookup_is_pinned_to_this_worker_and_the_newest_dispatch() {
        let sql = dispatched_job_query(WORKER, JOB)
            .build(DatabaseBackend::Postgres)
            .to_string();

        assert!(sql.contains("\"job_id\" = "), "{sql}");
        assert!(sql.contains("\"worker_id\" = "), "{sql}");
        assert!(sql.contains("ORDER BY"), "{sql}");
        assert!(sql.contains("\"dispatched_at\" DESC"), "{sql}");
    }
}
