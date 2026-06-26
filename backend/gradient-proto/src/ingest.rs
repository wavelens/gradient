/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::StorePath;
use gradient_util::nix_hash::{is_nix32_hash, normalize_nar_hash};
use gradient_storage::nar::NarStore;
use gradient_types::ids::{CacheId, CachedPathId, CachedPathSignatureId, OrganizationId};
use gradient_types::*;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel, QueryFilter, Set,
};
use tracing::{debug, warn};

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
    /// Record the path but enqueue no signatures (no resolvable org).
    None,
}

/// Outcome of an ingest.
pub struct IngestOutcome {
    pub cached_path: CachedPathId,
    /// True when the `cached_path` row was created by this call.
    pub created: bool,
}

fn parse_store_path(store_path: &str) -> anyhow::Result<StorePath> {
    let sp = StorePath::parse(store_path).map_err(|e| anyhow::anyhow!("{e}"))?;
    if !is_nix32_hash(sp.hash()) {
        anyhow::bail!("malformed store path: {}", store_path);
    }

    Ok(sp)
}

pub async fn ingest_nar<C: ConnectionTrait>(
    db: &C,
    nar_storage: &NarStore,
    nar_bytes: Vec<u8>,
    input: IngestInput<'_>,
    targets: SignTargets,
) -> anyhow::Result<IngestOutcome> {
    let sp = parse_store_path(input.store_path)?;
    // NAR written first; DB failure leaves an unreferenced blob - GC reclaims it.
    put_nar_idempotent(db, nar_storage, sp.hash(), input.file_hash, nar_bytes).await?;
    upsert_and_sign(db, sp.hash(), sp.name(), input, targets).await
}

/// Store `nar_bytes` for store-path `hash`, skipping the object-store write when
/// the identical NAR is already present: a `cached_path` row records the same
/// compressed `file_hash` AND the object is physically there (`HEAD`). A re-push
/// of unchanged content is then a metadata-only no-op instead of a fresh `PUT`,
/// which on a versioning-enabled bucket would otherwise pile up retained
/// versions that no S3-API GC can reclaim. `file_hash` is the incoming
/// compressed-NAR hash (`sha256:<nix32>`); returns whether bytes were written.
pub async fn put_nar_idempotent<C: ConnectionTrait>(
    db: &C,
    nar_storage: &NarStore,
    hash: &str,
    file_hash: &str,
    nar_bytes: Vec<u8>,
) -> anyhow::Result<bool> {
    let incoming = normalize_nar_hash(file_hash);
    let recorded_match = ECachedPath::find()
        .filter(CCachedPath::Hash.eq(hash))
        .one(db)
        .await?
        .and_then(|row| row.file_hash)
        .is_some_and(|fh| fh == incoming);

    if recorded_match && nar_storage.exists(hash).await? {
        debug!(%hash, "NAR already stored with matching file_hash; skipping re-upload");
        return Ok(false);
    }

    nar_storage.put(hash, nar_bytes).await?;
    Ok(true)
}

pub async fn ingest_metadata_only<C: ConnectionTrait>(
    db: &C,
    input: IngestInput<'_>,
    targets: SignTargets,
) -> anyhow::Result<IngestOutcome> {
    let sp = parse_store_path(input.store_path)?;
    upsert_and_sign(db, sp.hash(), sp.name(), input, targets).await
}

/// Record a path's hash-name references in the normalized `cached_path_reference`
/// relation: `reference_hash` indexes referrer lookups, and `position` preserves
/// the worker's order (nix store-path order) so the narinfo `References:` line and
/// signature fingerprint reconstruct verbatim. Content-addressed, so re-ingest is
/// a no-op.
async fn sync_reference_index<C: ConnectionTrait>(
    db: &C,
    hash: &str,
    references: &[String],
) -> Result<(), sea_orm::DbErr> {
    db.execute(sea_orm::Statement::from_sql_and_values(
        sea_orm::DatabaseBackend::Postgres,
        r#"
        INSERT INTO cached_path_reference (id, referrer, reference, reference_hash, position)
        SELECT uuidv7(), $1, t.tok, split_part(t.tok, '-', 1), t.ord
        FROM unnest($2::text[]) WITH ORDINALITY AS t(tok, ord)
        WHERE t.tok <> ''
        ON CONFLICT (referrer, reference) DO NOTHING
        "#,
        [hash.into(), references.to_vec().into()],
    ))
    .await?;

    Ok(())
}

async fn upsert_and_sign<C: ConnectionTrait>(
    db: &C,
    hash: &str,
    package: &str,
    input: IngestInput<'_>,
    targets: SignTargets,
) -> anyhow::Result<IngestOutcome> {
    let ts = now();

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
            if input.deriver.is_some() {
                active.deriver = Set(input.deriver.map(str::to_owned));
            }
            active.update(db).await?;
            (id, false)
        }
        None => {
            let am = MCachedPath {
                id: CachedPathId::now_v7(),
                hash: hash.to_owned(),
                package: package.to_owned(),
                file_hash: Some(normalize_nar_hash(input.file_hash)),
                file_size: Some(input.file_size),
                nar_size: Some(input.nar_size),
                nar_hash: Some(normalize_nar_hash(input.nar_hash)),
                deriver: input.deriver.map(str::to_owned),
                created_at: ts,
                ..Default::default()
            }
            .into_active_model();

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

    if !input.references.is_empty() {
        sync_reference_index(db, hash, input.references).await?;
    }

    let cache_ids: Vec<CacheId> = match targets {
        SignTargets::None => vec![],
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
            .map(|cid| {
                MCachedPathSignature {
                    id: CachedPathSignatureId::now_v7(),
                    cached_path: cached_path_id,
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
    fn returned_cached_path(hash: &str) -> gradient_entity::cached_path::Model {
        gradient_entity::cached_path::Model {
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
            // `put_nar_idempotent` looks up the path first; no row ⇒ write.
            .append_query_results([Vec::<gradient_entity::cached_path::Model>::new()])
            .append_query_results([Vec::<gradient_entity::cached_path::Model>::new()])
            .append_query_results([vec![returned_cached_path(hash)]])
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

    const IDEM_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn row_with_file_hash(file_hash: &str) -> gradient_entity::cached_path::Model {
        let mut row = returned_cached_path(IDEM_HASH);
        row.file_hash = Some(normalize_nar_hash(file_hash));
        row
    }

    /// Identical content already present (matching `file_hash` + object on
    /// disk) ⇒ the write is skipped and the stored bytes are left untouched.
    #[tokio::test]
    async fn idempotent_skips_when_present_and_hash_matches() {
        let store = temp_store();
        store.put(IDEM_HASH, b"OLD".to_vec()).await.unwrap();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row_with_file_hash("sha256:abc")]])
            .into_connection();

        let wrote = put_nar_idempotent(&db, &store, IDEM_HASH, "sha256:abc", b"NEW".to_vec())
            .await
            .unwrap();
        assert!(!wrote, "must skip when an identical NAR is already stored");
        assert_eq!(store.get(IDEM_HASH).await.unwrap().unwrap(), b"OLD");
    }

    /// No `cached_path` row ⇒ first write goes through.
    #[tokio::test]
    async fn idempotent_writes_when_no_row() {
        let store = temp_store();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<gradient_entity::cached_path::Model>::new()])
            .into_connection();

        let wrote = put_nar_idempotent(&db, &store, IDEM_HASH, "sha256:abc", b"NEW".to_vec())
            .await
            .unwrap();
        assert!(wrote);
        assert_eq!(store.get(IDEM_HASH).await.unwrap().unwrap(), b"NEW");
    }

    /// A recorded but *different* `file_hash` means the content changed
    /// (non-reproducible rebuild) ⇒ overwrite, never serve stale bytes.
    #[tokio::test]
    async fn idempotent_writes_when_hash_differs() {
        let store = temp_store();
        store.put(IDEM_HASH, b"OLD".to_vec()).await.unwrap();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row_with_file_hash("sha256:different")]])
            .into_connection();

        let wrote = put_nar_idempotent(&db, &store, IDEM_HASH, "sha256:abc", b"NEW".to_vec())
            .await
            .unwrap();
        assert!(wrote);
        assert_eq!(store.get(IDEM_HASH).await.unwrap().unwrap(), b"NEW");
    }

    /// Matching `file_hash` but the object is gone (zombie row) ⇒ re-write so
    /// the row⟺object invariant is restored.
    #[tokio::test]
    async fn idempotent_writes_when_object_missing() {
        let store = temp_store();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row_with_file_hash("sha256:abc")]])
            .into_connection();

        let wrote = put_nar_idempotent(&db, &store, IDEM_HASH, "sha256:abc", b"NEW".to_vec())
            .await
            .unwrap();
        assert!(wrote, "a zombie row whose object is gone must re-write");
        assert_eq!(store.get(IDEM_HASH).await.unwrap().unwrap(), b"NEW");
    }
}
