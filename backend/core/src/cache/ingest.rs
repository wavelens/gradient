/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::nix_hash::{is_nix32_hash, normalize_nar_hash};
use crate::storage::nar::NarStore;
use crate::types::ids::{CacheId, CachedPathId, CachedPathSignatureId, OrganizationId};
use crate::types::*;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel, QueryFilter, Set,
};
use tracing::warn;

/// NAR metadata required to record a cached path. Hashes are normalized on write.
pub struct IngestInput<'a> {
    pub store_path: &'a str,
    pub file_hash: &'a str,
    pub file_size: i64,
    pub nar_size: i64,
    pub nar_hash: &'a str,
    /// References in hash-name format (no `/nix/store/` prefix).
    pub references: &'a [String],
    pub deriver: Option<&'a str>,
}

pub enum SignTargets {
    OrgCaches(OrganizationId),
    Cache(CacheId),
}

/// Outcome of an ingest.
pub struct IngestOutcome {
    pub cached_path: CachedPathId,
    /// True when the `cached_path` row was created by this call.
    pub created: bool,
}

fn parse_store_hash(store_path: &str) -> anyhow::Result<(&str, &str)> {
    let hash_name = store_path.strip_prefix("/nix/store/").unwrap_or(store_path);
    let hash = hash_name.split('-').next().unwrap_or("");
    let package = hash_name.find('-').map(|i| &hash_name[i + 1..]).unwrap_or("");
    if !is_nix32_hash(hash) {
        anyhow::bail!("malformed store path: {}", store_path);
    }
    Ok((hash, package))
}

pub async fn ingest_nar<C: ConnectionTrait>(
    db: &C,
    nar_storage: &NarStore,
    nar_bytes: Vec<u8>,
    input: IngestInput<'_>,
    targets: SignTargets,
) -> anyhow::Result<IngestOutcome> {
    let (hash, package) = parse_store_hash(input.store_path)?;
    // NAR written first; DB failure leaves an unreferenced blob — GC reclaims it.
    nar_storage.put(hash, nar_bytes).await?;
    upsert_and_sign(db, hash, package, input, targets).await
}

pub async fn ingest_metadata_only<C: ConnectionTrait>(
    db: &C,
    input: IngestInput<'_>,
    targets: SignTargets,
) -> anyhow::Result<IngestOutcome> {
    let (hash, package) = parse_store_hash(input.store_path)?;
    upsert_and_sign(db, hash, package, input, targets).await
}

async fn upsert_and_sign<C: ConnectionTrait>(
    db: &C,
    hash: &str,
    package: &str,
    input: IngestInput<'_>,
    targets: SignTargets,
) -> anyhow::Result<IngestOutcome> {
    let ts = now();
    let references_str = if input.references.is_empty() {
        None
    } else {
        Some(input.references.join(" "))
    };

    let (cached_path_id, created) = match ECachedPath::find()
        .filter(CCachedPath::Hash.eq(hash))
        .one(db)
        .await?
    {
        Some(row) => {
            let id = row.id;
            let mut active = row.into_active_model();
            active.file_size = Set(Some(input.file_size));
            active.file_hash = Set(Some(normalize_nar_hash(input.file_hash)));
            active.nar_size = Set(Some(input.nar_size));
            active.nar_hash = Set(Some(normalize_nar_hash(input.nar_hash)));
            if references_str.is_some() {
                active.references = Set(references_str);
            }
            if input.deriver.is_some() {
                active.deriver = Set(input.deriver.map(str::to_owned));
            }
            active.update(db).await?;
            (id, false)
        }
        None => {
            let am = ACachedPath {
                id: Set(CachedPathId::now_v7()),
                store_path: Set(input.store_path.to_owned()),
                hash: Set(hash.to_owned()),
                package: Set(package.to_owned()),
                file_hash: Set(Some(normalize_nar_hash(input.file_hash))),
                file_size: Set(Some(input.file_size)),
                nar_size: Set(Some(input.nar_size)),
                nar_hash: Set(Some(normalize_nar_hash(input.nar_hash))),
                references: Set(references_str),
                ca: Set(None),
                deriver: Set(input.deriver.map(str::to_owned)),
                created_at: Set(ts),
            };
            match am.insert(db).await {
                Ok(row) => (row.id, true),
                Err(e) => {
                    warn!(store_path = input.store_path, error = %e, "insert cached_path failed (possible race)");
                    match ECachedPath::find()
                        .filter(CCachedPath::Hash.eq(hash))
                        .one(db)
                        .await?
                    {
                        Some(row) => (row.id, false),
                        None => return Err(e.into()),
                    }
                }
            }
        }
    };

    let cache_ids: Vec<CacheId> = match targets {
        SignTargets::Cache(id) => vec![id],
        SignTargets::OrgCaches(org) => EOrganizationCache::find()
            .filter(COrganizationCache::Organization.eq(org))
            .all(db)
            .await?
            .into_iter()
            .map(|oc| oc.cache)
            .collect(),
    };

    if !cache_ids.is_empty() {
        let rows: Vec<ACachedPathSignature> = cache_ids
            .into_iter()
            .map(|cid| ACachedPathSignature {
                id: Set(CachedPathSignatureId::now_v7()),
                cached_path: Set(cached_path_id),
                cache: Set(cid),
                signature: Set(None),
                created_at: Set(ts),
                last_fetched_at: Set(None),
                fetch_count: Set(0),
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
            .do_nothing()
            .exec(db)
            .await;
        if let Err(e) = result {
            warn!(store_path = input.store_path, error = %e, "insert cached_path_signature failed");
        }
    }

    Ok(IngestOutcome { cached_path: cached_path_id, created })
}

#[cfg(test)]
mod tests {
    use super::*;
    use entity::ids::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use uuid::Uuid;

    fn temp_store() -> NarStore {
        let dir = std::env::temp_dir().join(format!("gradient-ingest-{}", Uuid::now_v7()));
        NarStore::local(dir.to_str().unwrap()).expect("local store")
    }
    fn cache_id() -> CacheId {
        CacheId::new(Uuid::parse_str("10000000-0000-0000-0000-000000000002").unwrap())
    }
    fn input(store_path: &str) -> IngestInput<'_> {
        IngestInput {
            store_path,
            file_hash: "sha256:abc",
            file_size: 5,
            nar_size: 5,
            nar_hash: "sha256:def",
            references: &[],
            deriver: None,
        }
    }
    fn returned_cached_path(store_path: &str, hash: &str) -> entity::cached_path::Model {
        entity::cached_path::Model {
            id: CachedPathId::new(Uuid::now_v7()),
            store_path: store_path.to_string(),
            hash: hash.to_string(),
            package: "hello-2.12".to_string(),
            file_hash: Some("sha256:abc".to_string()),
            file_size: Some(5),
            nar_size: Some(5),
            nar_hash: Some("sha256:def".to_string()),
            references: None,
            ca: None,
            deriver: None,
            created_at: now(),
        }
    }

    #[tokio::test]
    async fn malformed_store_path_bails_before_any_io() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let store = temp_store();
        let err = ingest_nar(
            &db,
            &store,
            vec![1],
            input("not-a-store-path"),
            SignTargets::Cache(cache_id()),
        )
        .await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn create_path_writes_blob_and_reports_created() {
        let sp = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello-2.12";
        let hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<entity::cached_path::Model>::new()])
            .append_query_results([vec![returned_cached_path(sp, hash)]])
            .append_exec_results([MockExecResult { last_insert_id: 0, rows_affected: 1 }])
            .into_connection();
        let store = temp_store();
        let out = ingest_nar(
            &db,
            &store,
            vec![1, 2, 3, 4, 5],
            input(sp),
            SignTargets::Cache(cache_id()),
        )
        .await
        .expect("ingest");
        assert!(out.created);
        let blob = store.get(hash).await.expect("get").expect("present");
        assert_eq!(blob, vec![1, 2, 3, 4, 5]);
    }
}
