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
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};
use tokio::io::AsyncRead;
use tracing::debug;

pub use gradient_graph::{NarCommit, NarCommitted, SignTargets};

pub struct ImportInput<'a> {
    pub store_path: &'a str,
    pub file_hash: &'a str,
    pub file_size: i64,
    pub nar_size: i64,
    pub nar_hash: &'a str,
    pub references: &'a [String],
    pub deriver: Option<&'a str>,
    pub ca: Option<&'a str>,
}

impl ImportInput<'_> {
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
            confirmed: true,
            built_by_worker: false,
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

pub async fn import_nar_reader<C, R>(
    db: &C,
    nar_storage: &NarStore,
    graph: &Graph,
    reader: R,
    input: ImportInput<'_>,
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
    if nar_write_needed(db, nar_storage, hash, file_hash).await? != WriteNeeded::Write {
        return Ok(false);
    }

    nar_storage.put_reader(hash, reader).await?;
    Ok(true)
}

pub async fn stored_path<C: ConnectionTrait>(
    db: &C,
    hash: &str,
) -> Result<Option<MCachedPath>, DbErr> {
    Ok(ECachedPath::find()
        .filter(CCachedPath::Hash.eq(hash))
        .one(db)
        .await?
        .filter(MCachedPath::is_stored))
}

pub async fn nar_write_needed<C: ConnectionTrait>(
    db: &C,
    nar_storage: &NarStore,
    hash: &str,
    file_hash: &str,
) -> anyhow::Result<WriteNeeded> {
    let Some(row) = stored_path(db, hash).await? else {
        return Ok(WriteNeeded::Write);
    };

    if row.file_hash.as_deref().map(normalize_nar_hash) != Some(normalize_nar_hash(file_hash)) {
        debug!(%hash, "other content is stored for this path; keeping it");
        return Ok(WriteNeeded::OtherContentStored);
    }

    if nar_storage.exists(hash).await? {
        return Ok(WriteNeeded::Stored);
    }

    Ok(WriteNeeded::Write)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteNeeded {
    Write,
    Stored,
    OtherContentStored,
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_types::ids::{CacheId, CachedPathId};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use uuid::Uuid;

    fn temp_store() -> NarStore {
        let dir = std::env::temp_dir().join(format!("gradient-import-{}", Uuid::now_v7()));
        NarStore::local(dir.to_str().unwrap()).expect("local store")
    }
    fn cache_id() -> CacheId {
        CacheId::new(Uuid::parse_str("10000000-0000-0000-0000-000000000002").unwrap())
    }
    fn input(store_path: &str) -> ImportInput<'_> {
        ImportInput {
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
            confirmed: true,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn malformed_store_path_bails_before_any_io() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let store = temp_store();
        let err = import_nar_reader(
            &db,
            &store,
            &Graph::stub(),
            &b"x"[..],
            input("not-a-store-path"),
            SignTargets::Cache(cache_id()),
        )
        .await;
        assert!(err.is_err());
    }

    const IDEM_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn row_with_file_hash(file_hash: &str) -> gradient_entity::cached_path::Model {
        let mut row = returned_cached_path(IDEM_HASH);
        row.file_hash = Some(normalize_nar_hash(file_hash));
        row
    }

    async fn put(db: &sea_orm::DatabaseConnection, store: &NarStore) -> anyhow::Result<bool> {
        put_nar_idempotent_reader(db, store, IDEM_HASH, "sha256:abc", &b"NEW"[..]).await
    }

    #[tokio::test]
    async fn an_identical_stored_nar_is_not_written_again() {
        let store = temp_store();
        store.put(IDEM_HASH, b"OLD".to_vec()).await.unwrap();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row_with_file_hash("sha256:abc")]])
            .into_connection();

        assert!(!put(&db, &store).await.unwrap());
        assert_eq!(store.get(IDEM_HASH).await.unwrap().unwrap(), b"OLD");
    }

    #[tokio::test]
    async fn a_path_without_a_row_is_written() {
        let store = temp_store();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<gradient_entity::cached_path::Model>::new()])
            .into_connection();

        assert!(put(&db, &store).await.unwrap());
        assert_eq!(store.get(IDEM_HASH).await.unwrap().unwrap(), b"NEW");
    }

    #[tokio::test]
    async fn a_stored_path_with_other_content_is_never_overwritten() {
        let store = temp_store();
        store.put(IDEM_HASH, b"OLD".to_vec()).await.unwrap();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row_with_file_hash("sha256:different")]])
            .into_connection();

        assert!(!put(&db, &store).await.unwrap());
        assert_eq!(store.get(IDEM_HASH).await.unwrap().unwrap(), b"OLD");
    }

    #[tokio::test]
    async fn an_identical_row_whose_object_is_gone_is_written_again() {
        let store = temp_store();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row_with_file_hash("sha256:abc")]])
            .into_connection();

        assert!(put(&db, &store).await.unwrap());
        assert_eq!(store.get(IDEM_HASH).await.unwrap().unwrap(), b"NEW");
    }

    #[tokio::test]
    async fn a_failed_lookup_writes_nothing() {
        use sea_orm::{DbErr, RuntimeErr};
        let store = temp_store();
        store.put(IDEM_HASH, b"OLD".to_vec()).await.unwrap();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors(vec![DbErr::Conn(RuntimeErr::Internal(
                "Connection pool timed out".to_string(),
            ))])
            .into_connection();

        assert!(put(&db, &store).await.is_err());
        assert_eq!(store.get(IDEM_HASH).await.unwrap().unwrap(), b"OLD");
    }
}
