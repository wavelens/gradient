/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Recording a stored NAR in the cache index.

use anyhow::Context as _;
use gradient_db::{DbContext, WorkerDb};
use gradient_entity::StorePath;
use gradient_types::ids::{CacheId, CachedPathId, CachedPathSignatureId};
use gradient_types::*;
use gradient_util::nix_hash::{is_nix32_hash, normalize_nar_hash};
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel, QueryFilter, Set,
};
use tracing::{debug, warn};

use crate::messages::{NarCommit, NarCommitted, SignTargets};

pub(crate) async fn commit(db: &WorkerDb, c: &NarCommit) -> anyhow::Result<NarCommitted> {
    let sp = StorePath::parse(&c.store_path).map_err(|e| anyhow::anyhow!("{e}"))?;
    if !is_nix32_hash(sp.hash()) {
        anyhow::bail!("malformed store path: {}", c.store_path);
    }

    let (cached_path, created) = upsert_cached_path(db, sp.hash(), sp.name(), c).await?;
    if !c.references.is_empty() {
        sync_reference_index(db, sp.hash(), &c.references).await?;
    }

    queue_signature_placeholders(db, cached_path, c.targets).await?;
    let outputs_marked = EDerivationOutput::update_many()
        .col_expr(CDerivationOutput::IsCached, Expr::value(true))
        .col_expr(CDerivationOutput::CachedPath, Expr::value(cached_path))
        .filter(CDerivationOutput::Hash.eq(sp.hash()))
        .exec(db)
        .await
        .context("mark derivation outputs cached")?
        .rows_affected;
    debug!(store_path = %c.store_path, outputs_marked, created, "cached path committed");

    Ok(NarCommitted {
        cached_path,
        created,
        outputs_marked,
    })
}

/// The debug-info walk decompresses the whole NAR, so it runs detached.
pub(crate) fn after_commit(ctx: &DbContext, committed: &NarCommitted, store_path: &str) {
    let Ok(sp) = StorePath::parse(store_path) else {
        return;
    };
    if !gradient_db::carries_debug_info(sp.name()) {
        return;
    }

    let db = ctx.worker_db.detached();
    let nar_storage = ctx.storage.nar_storage.clone();
    let hash = sp.hash().to_owned();
    let cached_path = committed.cached_path;
    ctx.shutdown.spawn(async move {
        match gradient_db::index_cached_path(&db, &nar_storage, cached_path, &hash).await {
            Ok(0) => {}
            Ok(count) => debug!(%hash, count, "indexed debug-info build ids"),
            Err(e) => warn!(%hash, error = %e, "failed to index debug info"),
        }
    });
}

async fn upsert_cached_path(
    db: &WorkerDb,
    hash: &str,
    package: &str,
    c: &NarCommit,
) -> anyhow::Result<(CachedPathId, bool)> {
    match ECachedPath::find()
        .filter(CCachedPath::Hash.eq(hash))
        .one(db)
        .await?
    {
        Some(row) => {
            let id = row.id;
            let file_hash = normalize_nar_hash(&c.file_hash);
            // Different bytes under the same store path: the recorded build-id
            // members no longer describe the NAR, so re-open it to the indexer.
            let rescan_debug_info = row.file_hash.as_deref() != Some(file_hash.as_str());
            let mut active = row.into_active_model();
            active.file_size = Set(Some(c.file_size));
            active.file_hash = Set(Some(file_hash));
            if rescan_debug_info {
                active.debug_info_indexed = Set(false);
            }

            active.nar_size = Set(Some(c.nar_size));
            active.nar_hash = Set(Some(normalize_nar_hash(&c.nar_hash)));
            if c.deriver.is_some() {
                active.deriver = Set(c.deriver.clone());
            }

            if c.ca.is_some() {
                active.ca = Set(c.ca.clone());
            }

            active.update(db).await?;
            Ok((id, false))
        }
        None => {
            let am = MCachedPath {
                id: CachedPathId::now_v7(),
                hash: hash.to_owned(),
                package: package.to_owned(),
                file_hash: Some(normalize_nar_hash(&c.file_hash)),
                file_size: Some(c.file_size),
                nar_size: Some(c.nar_size),
                nar_hash: Some(normalize_nar_hash(&c.nar_hash)),
                deriver: c.deriver.clone(),
                ca: c.ca.clone(),
                created_at: now(),
                ..Default::default()
            }
            .into_active_model();

            match am.insert(db).await {
                Ok(row) => Ok((row.id, true)),
                Err(e) => {
                    warn!(store_path = %c.store_path, error = %e, "insert cached_path failed (possible race)");
                    match ECachedPath::find()
                        .filter(CCachedPath::Hash.eq(hash))
                        .one(db)
                        .await?
                    {
                        Some(row) => Ok((row.id, false)),
                        None => Err(e.into()),
                    }
                }
            }
        }
    }
}

/// Record a path's hash-name references in the normalized `cached_path_reference`
/// relation: `reference_hash` indexes referrer lookups, and `position` preserves
/// the worker's order (nix store-path order) so the narinfo `References:` line and
/// signature fingerprint reconstruct verbatim. Content-addressed, so re-ingest is
/// a no-op.
async fn sync_reference_index(
    db: &WorkerDb,
    hash: &str,
    references: &[String],
) -> Result<(), sea_orm::DbErr> {
    db.execute_raw(sea_orm::Statement::from_sql_and_values(
        sea_orm::DatabaseBackend::Postgres,
        r#"
        INSERT INTO cached_path_reference (referrer, reference, reference_hash, position)
        SELECT $1, t.tok, split_part(t.tok, '-', 1), t.ord
        FROM unnest($2::text[]) WITH ORDINALITY AS t(tok, ord)
        WHERE t.tok <> ''
        ON CONFLICT (referrer, reference) DO NOTHING
        "#,
        [hash.into(), references.to_vec().into()],
    ))
    .await?;

    Ok(())
}

async fn queue_signature_placeholders(
    db: &WorkerDb,
    cached_path: CachedPathId,
    targets: SignTargets,
) -> anyhow::Result<()> {
    let cache_ids: Vec<CacheId> = match targets {
        SignTargets::None => vec![],
        SignTargets::Cache(id) => vec![id],
        SignTargets::ProjectCaches(project) => EProjectCache::find()
            .filter(CProjectCache::Project.eq(project))
            .all(db)
            .await?
            .into_iter()
            .map(|oc| oc.cache)
            .collect(),
    };

    if cache_ids.is_empty() {
        return Ok(());
    }

    let ts = now();
    let rows: Vec<ACachedPathSignature> = cache_ids
        .into_iter()
        .map(|cid| {
            MCachedPathSignature {
                id: CachedPathSignatureId::now_v7(),
                cached_path,
                cache: cid,
                created_at: ts,
                ..Default::default()
            }
            .into_active_model()
        })
        .collect();

    let result = ECachedPathSignature::insert_many(rows)
        .on_conflict(
            OnConflict::columns([
                CCachedPathSignature::CachedPath,
                CCachedPathSignature::Cache,
            ])
            .do_nothing()
            .to_owned(),
        )
        .try_insert()
        .exec(db)
        .await;
    if let Err(e) = result {
        warn!(%cached_path, error = %e, "insert cached_path_signature failed");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_types::ids::ProjectId;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use uuid::Uuid;

    const SP: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello-2.12";
    const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn cache_id() -> CacheId {
        CacheId::new(Uuid::parse_str("10000000-0000-0000-0000-000000000002").unwrap())
    }

    fn project() -> ProjectId {
        ProjectId::new(Uuid::parse_str("20000000-0000-0000-0000-000000000003").unwrap())
    }

    fn project_cache_row() -> gradient_entity::project_cache::Model {
        gradient_entity::project_cache::Model {
            project: project(),
            cache: cache_id(),
            ..Default::default()
        }
    }

    fn returned_cached_path(hash: &str) -> MCachedPath {
        MCachedPath {
            id: CachedPathId::new(Uuid::now_v7()),
            hash: hash.to_string(),
            package: "hello-2.12".to_string(),
            file_hash: Some("sha256:abc".to_string()),
            file_size: Some(5),
            nar_size: Some(5),
            nar_hash: Some("sha256:def".to_string()),
            created_at: now(),
            ..Default::default()
        }
    }

    fn commit_for(store_path: &str) -> NarCommit {
        NarCommit {
            store_path: store_path.to_owned(),
            file_hash: "sha256:abc".to_owned(),
            file_size: 5,
            nar_size: 5,
            nar_hash: "sha256:def".to_owned(),
            references: Vec::new(),
            deriver: None,
            ca: None,
            targets: SignTargets::None,
        }
    }

    fn log_has_signature_insert(db: WorkerDb) -> bool {
        db.into_transaction_log()
            .iter()
            .any(|t| format!("{t:?}").contains("cached_path_signature"))
    }

    /// A resolved project enqueues a `cached_path_signature` placeholder for every
    /// subscribed cache. Regression guard: the detached NAR commit must resolve
    /// the project on the read loop before the job is evicted from the tracker,
    /// otherwise `SignTargets` collapses to `None`, no placeholder is written,
    /// the sign sweep has nothing to sign, and the narinfo 404s forever.
    #[tokio::test]
    async fn project_target_enqueues_signature_placeholder() {
        let db = WorkerDb::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<MCachedPath>::new()])
                .append_query_results([vec![returned_cached_path(HASH)]])
                .append_query_results([vec![project_cache_row()]])
                .append_exec_results([
                    MockExecResult {
                        last_insert_id: 0,
                        rows_affected: 1,
                    },
                    MockExecResult {
                        last_insert_id: 0,
                        rows_affected: 1,
                    },
                ])
                .into_connection(),
        );

        commit(
            &db,
            &NarCommit {
                targets: SignTargets::ProjectCaches(project()),
                ..commit_for(SP)
            },
        )
        .await
        .expect("commit");

        assert!(
            log_has_signature_insert(db),
            "ProjectCaches target must insert a cached_path_signature placeholder"
        );
    }

    #[tokio::test]
    async fn ingest_records_content_address() {
        let ca = "text:sha256:006vc8gixyrcynsx4lz1qxingl0mdja3l0xw1nl0j73isg37x944";
        let db = WorkerDb::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<MCachedPath>::new()])
                .append_query_results([vec![returned_cached_path(HASH)]])
                .append_exec_results([MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 0,
                }])
                .into_connection(),
        );

        commit(
            &db,
            &NarCommit {
                ca: Some(ca.to_owned()),
                ..commit_for(SP)
            },
        )
        .await
        .expect("commit");

        let logged = db
            .into_transaction_log()
            .iter()
            .any(|t| format!("{t:?}").contains(ca));
        assert!(logged, "the content address must be written to cached_path");
    }

    /// No resolvable project records the path but enqueues no signature, so the
    /// endpoint can distinguish "not yet signed" from "will never be signed".
    #[tokio::test]
    async fn none_target_enqueues_no_signature() {
        let db = WorkerDb::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<MCachedPath>::new()])
                .append_query_results([vec![returned_cached_path(HASH)]])
                .append_exec_results([MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 0,
                }])
                .into_connection(),
        );

        commit(&db, &commit_for(SP)).await.expect("commit");

        assert!(
            !log_has_signature_insert(db),
            "None target must not touch cached_path_signature"
        );
    }

    #[tokio::test]
    async fn a_committed_path_backs_every_output_with_its_hash() {
        let db = WorkerDb::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<MCachedPath>::new()])
                .append_query_results([vec![returned_cached_path(HASH)]])
                .append_exec_results([MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 2,
                }])
                .into_connection(),
        );

        let committed = commit(&db, &commit_for(SP)).await.unwrap();
        assert!(committed.created);
        assert_eq!(committed.outputs_marked, 2);
    }
}
