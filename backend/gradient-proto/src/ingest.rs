/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::StorePath;
use gradient_graph::Graph;
use gradient_storage::nar::NarStore;
use gradient_types::*;
use gradient_util::nix_hash::{is_nix32_hash, normalize_nar_hash};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use tokio::io::AsyncRead;
use tracing::{debug, warn};

pub use gradient_graph::{NarCommit, NarCommitted, SignTargets};

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
    /// Content address in narinfo form (`text:sha256:<b32>` /
    /// `fixed:[r:]sha256:<b32>`), if the path is content-addressed.
    pub ca: Option<&'a str>,
}

impl IngestInput<'_> {
    pub fn to_commit(&self, targets: SignTargets) -> NarCommit {
        NarCommit {
            store_path: self.store_path.to_owned(),
            file_hash: self.file_hash.to_owned(),
            file_size: self.file_size,
            nar_size: self.nar_size,
            nar_hash: self.nar_hash.to_owned(),
            references: self.references.to_vec(),
            deriver: self.deriver.map(str::to_owned),
            ca: self.ca.map(str::to_owned),
            targets,
        }
    }
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
    graph: &Graph,
    nar_bytes: Vec<u8>,
    input: IngestInput<'_>,
    targets: SignTargets,
) -> anyhow::Result<NarCommitted> {
    let sp = parse_store_path(input.store_path)?;
    // NAR written first; DB failure leaves an unreferenced blob - GC reclaims it.
    put_nar_idempotent(db, nar_storage, sp.hash(), input.file_hash, nar_bytes).await?;
    graph.commit_nar(input.to_commit(targets)).await
}

/// Streaming counterpart to [`ingest_nar`]: takes an `AsyncRead` over the
/// compressed NAR (typically a staged `.partial` file) so the whole object is
/// never buffered in memory.
pub async fn ingest_nar_reader<C, R>(
    db: &C,
    nar_storage: &NarStore,
    graph: &Graph,
    reader: R,
    input: IngestInput<'_>,
    targets: SignTargets,
) -> anyhow::Result<NarCommitted>
where
    C: ConnectionTrait,
    R: AsyncRead + Unpin + Send,
{
    let sp = parse_store_path(input.store_path)?;
    put_nar_idempotent_reader(db, nar_storage, sp.hash(), input.file_hash, reader).await?;
    graph.commit_nar(input.to_commit(targets)).await
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
    if !nar_write_needed(db, nar_storage, hash, file_hash).await? {
        return Ok(false);
    }
    nar_storage.put(hash, nar_bytes).await?;
    Ok(true)
}

/// Streaming counterpart to [`put_nar_idempotent`]: same idempotency skip, but
/// streams `reader` into storage via multipart instead of taking a `Vec<u8>`.
/// On a skip the reader is dropped unread.
pub async fn put_nar_idempotent_reader<C, R>(
    db: &C,
    nar_storage: &NarStore,
    hash: &str,
    file_hash: &str,
    reader: R,
) -> anyhow::Result<bool>
where
    C: ConnectionTrait,
    R: AsyncRead + Unpin + Send,
{
    if !nar_write_needed(db, nar_storage, hash, file_hash).await? {
        return Ok(false);
    }
    nar_storage.put_reader(hash, reader).await?;
    Ok(true)
}

/// Whether `hash`'s NAR must be (re)written, or can be skipped because an
/// identical one is already stored: a `cached_path` row records the same
/// compressed `file_hash` AND the object is physically present (`HEAD`). A
/// re-push of unchanged content is then a metadata-only no-op instead of a
/// fresh write, which on a versioning-enabled bucket would otherwise pile up
/// retained versions that no S3-API GC can reclaim. `file_hash` is the incoming
/// compressed-NAR hash (`sha256:<nix32>`).
///
/// The lookup is a best-effort optimization. A transient DB error here (e.g. a
/// worker_db pool timeout under an eval's input-.drv push storm) must NOT
/// propagate: it would abort the commit and terminally fail an eval whose
/// evaluation actually succeeded. Degrade to "must write", which is always safe
/// (re-storing identical bytes is a no-op on the store side).
async fn nar_write_needed<C: ConnectionTrait>(
    db: &C,
    nar_storage: &NarStore,
    hash: &str,
    file_hash: &str,
) -> anyhow::Result<bool> {
    let incoming = normalize_nar_hash(file_hash);
    let recorded_match = match ECachedPath::find()
        .filter(CCachedPath::Hash.eq(hash))
        .one(db)
        .await
    {
        Ok(row) => row
            .and_then(|r| r.file_hash)
            .is_some_and(|fh| fh == incoming),
        Err(e) => {
            warn!(%hash, error = %e, "idempotency lookup failed; writing NAR unconditionally");
            false
        }
    };

    if recorded_match && nar_storage.exists(hash).await? {
        debug!(%hash, "NAR already stored with matching file_hash; skipping re-upload");
        return Ok(false);
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_types::ids::{CacheId, CachedPathId};
    use sea_orm::{DatabaseBackend, MockDatabase};
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
            ca: None,
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
            &Graph::stub(),
            vec![1],
            input("not-a-store-path"),
            SignTargets::Cache(cache_id()),
        )
        .await;
        assert!(err.is_err());
    }

    /// The blob half of an ingest: no `cached_path` row records the path, so the
    /// bytes are written to storage before the commit is handed to the actor.
    #[tokio::test]
    async fn create_path_writes_blob_and_reports_created() {
        let hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<gradient_entity::cached_path::Model>::new()])
            .into_connection();
        let store = temp_store();
        let wrote = put_nar_idempotent(&db, &store, hash, "sha256:abc", vec![1, 2, 3, 4, 5])
            .await
            .expect("put");
        assert!(wrote);
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

    /// A transient DB error on the idempotency lookup (pool timeout under an
    /// eval's .drv push storm) must degrade to an unconditional write, never
    /// propagate - propagation aborts the commit and terminally fails the eval.
    #[tokio::test]
    async fn idempotent_writes_when_lookup_errors() {
        use sea_orm::{DbErr, RuntimeErr};
        let store = temp_store();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors(vec![DbErr::Conn(RuntimeErr::Internal(
                "Connection pool timed out".to_string(),
            ))])
            .into_connection();

        let wrote = put_nar_idempotent(&db, &store, IDEM_HASH, "sha256:abc", b"NEW".to_vec())
            .await
            .expect("a transient lookup error must not fail the commit");
        assert!(
            wrote,
            "must write the NAR when the idempotency lookup errors"
        );
        assert_eq!(store.get(IDEM_HASH).await.unwrap().unwrap(), b"NEW");
    }

    /// The streaming put writes through when no row records the path, storing
    /// exactly the bytes read from the reader.
    #[tokio::test]
    async fn idempotent_reader_writes_when_no_row() {
        let store = temp_store();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<gradient_entity::cached_path::Model>::new()])
            .into_connection();

        let wrote = put_nar_idempotent_reader(&db, &store, IDEM_HASH, "sha256:abc", &b"NEW"[..])
            .await
            .unwrap();
        assert!(wrote);
        assert_eq!(store.get(IDEM_HASH).await.unwrap().unwrap(), b"NEW");
    }

    /// The streaming put honours the same idempotency skip as the buffered one:
    /// identical content already present leaves the stored bytes untouched and
    /// never consumes/streams the reader.
    #[tokio::test]
    async fn idempotent_reader_skips_when_present_and_hash_matches() {
        let store = temp_store();
        store.put(IDEM_HASH, b"OLD".to_vec()).await.unwrap();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row_with_file_hash("sha256:abc")]])
            .into_connection();

        let wrote = put_nar_idempotent_reader(&db, &store, IDEM_HASH, "sha256:abc", &b"NEW"[..])
            .await
            .unwrap();
        assert!(!wrote, "must skip when an identical NAR is already stored");
        assert_eq!(store.get(IDEM_HASH).await.unwrap().unwrap(), b"OLD");
    }
}
