/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::{StorePath, StorePathError};
use gradient_graph::Graph;
use gradient_storage::ServerComputedNarAndCompressedFileHashesAndSizes;
use gradient_storage::nar::NarStore;
use gradient_types::*;
use gradient_util::nix_hash::{is_nix32_hash, normalize_nar_hash};
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};
use tokio::io::AsyncRead;

pub use gradient_graph::{NarCommit, NarCommitContent, NarCommitted, StoredContentProducer};

pub struct ImportInput<'a> {
    pub store_path: &'a str,
    pub verified: &'a ServerComputedNarAndCompressedFileHashesAndSizes,
    pub references: &'a [String],
    pub deriver: Option<&'a str>,
    pub ca: Option<&'a str>,
}

impl ImportInput<'_> {
    pub fn to_commit(&self, producer: StoredContentProducer) -> NarCommit {
        NarCommit {
            store_path: self.store_path.to_owned(),
            content: NarCommitContent::ServerComputedNarAndCompressedFileHashesAndSizes(
                self.verified.clone(),
            ),
            references: self.references.to_vec(),
            deriver: self.deriver.map(str::to_owned),
            ca: self.ca.map(str::to_owned),
            producer,
        }
    }
}

fn parse_store_path(store_path: &str) -> anyhow::Result<StorePath> {
    let sp = StorePath::parse_stored(store_path)?;
    if !is_nix32_hash(sp.hash()) {
        anyhow::bail!(StorePathError::Malformed(store_path.to_owned()));
    }

    Ok(sp)
}

pub async fn import_nar_reader<R>(
    nar_storage: &NarStore,
    graph: &Graph,
    reader: R,
    input: ImportInput<'_>,
    producer: StoredContentProducer,
) -> anyhow::Result<NarCommitted>
where
    R: AsyncRead + Unpin + Send,
{
    let sp = parse_store_path(input.store_path)?;
    let key = input
        .verified
        .object_storage_key_for_store_path_hash_and_compressed_file_sha256(sp.hash());
    put_nar_idempotent_reader(nar_storage, &key, reader).await?;
    graph.commit_nar(input.to_commit(producer)).await
}

pub async fn put_nar_idempotent_reader<R>(
    nar_storage: &NarStore,
    key: &str,
    reader: R,
) -> anyhow::Result<bool>
where
    R: AsyncRead + Unpin + Send,
{
    if nar_storage.exists(key).await? {
        return Ok(false);
    }

    nar_storage.put_reader(key, reader).await?;
    Ok(true)
}

pub async fn find_verified_cached_path_by_store_hash<C: ConnectionTrait>(
    db: &C,
    hash: &str,
    nar_hash: &str,
    references_text: &str,
    ca: Option<&str>,
) -> Result<Option<MCachedPath>, DbErr> {
    let nar_hash = normalize_nar_hash(nar_hash);
    Ok(ECachedPath::find()
        .filter(CCachedPath::Hash.eq(hash))
        .all(db)
        .await?
        .into_iter()
        .find(|row| {
            row.is_server_verified_and_matches_nar_hash_references_text_and_content_address(
                &nar_hash,
                references_text,
                ca,
            )
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::mock_db;
    use gradient_types::ids::{CacheId, CachedPathId};
    use uuid::Uuid;

    fn temp_store() -> NarStore {
        let dir = std::env::temp_dir().join(format!("gradient-import-{}", Uuid::now_v7()));
        NarStore::local(&dir).expect("local store")
    }
    fn cache_id() -> CacheId {
        CacheId::new(Uuid::parse_str("10000000-0000-0000-0000-000000000002").unwrap())
    }
    fn verified(nar: &[u8]) -> ServerComputedNarAndCompressedFileHashesAndSizes {
        gradient_storage::compress_nar_bytes_and_verify_stream(nar)
    }
    fn returned_cached_path(hash: &str) -> gradient_entity::cached_path::Model {
        gradient_entity::cached_path::Model {
            id: CachedPathId::new(Uuid::now_v7()),
            hash: hash.to_string(),
            nar_object_storage_key: hash.to_string(),
            package: "hello-2.12".to_string(),
            file_hash: "sha256:abc".to_string(),
            file_size: Some(5),
            nar_size: 5,
            nar_hash: verified(b"nar").nar_hash().to_owned(),
            references: Some(String::new()),
            created_at: now(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn malformed_store_path_bails_before_any_io() {
        let store = temp_store();
        let facts = verified(b"x");
        let err = import_nar_reader(
            &store,
            &Graph::stub(),
            &b"x"[..],
            ImportInput {
                store_path: "not-a-store-path",
                verified: &facts,
                references: &[],
                deriver: None,
                ca: None,
            },
            StoredContentProducer::Upload {
                caches: vec![cache_id()],
                user: None,
                api_key: None,
                channel: gradient_entity::store_path_content_producer::CacheUploadChannel::RestHttpApiUpload,
            },
        )
        .await;
        assert!(err.is_err());
    }

    const IDEM_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn key_of(nar: &[u8]) -> String {
        verified(nar).object_storage_key_for_store_path_hash_and_compressed_file_sha256(IDEM_HASH)
    }

    #[tokio::test]
    async fn an_identical_stored_nar_is_not_written_again() {
        let store = temp_store();
        let key = key_of(b"NEW");
        store.put(&key, b"OLD".to_vec()).await.unwrap();

        assert!(
            !put_nar_idempotent_reader(&store, &key, &b"NEW"[..])
                .await
                .unwrap()
        );
        assert_eq!(store.get(&key).await.unwrap().unwrap(), b"OLD");
    }

    #[tokio::test]
    async fn writes_nar_bytes_to_content_variant_object_storage_key_when_no_object_exists() {
        let store = temp_store();
        let key = key_of(b"NEW");

        assert!(
            put_nar_idempotent_reader(&store, &key, &b"NEW"[..])
                .await
                .unwrap()
        );
        assert_eq!(store.get(&key).await.unwrap().unwrap(), b"NEW");
        assert!(store.get(IDEM_HASH).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_stored_path_with_other_content_is_never_overwritten() {
        let store = temp_store();
        let first = key_of(b"OLD");
        store.put(&first, b"OLD".to_vec()).await.unwrap();

        let second = key_of(b"NEW");
        assert!(
            put_nar_idempotent_reader(&store, &second, &b"NEW"[..])
                .await
                .unwrap()
        );
        assert_ne!(first, second);
        assert_eq!(store.get(&first).await.unwrap().unwrap(), b"OLD");
    }

    #[tokio::test]
    async fn find_verified_cached_path_by_store_hash_skips_unverified_row_with_same_hash() {
        let legacy = returned_cached_path(IDEM_HASH);
        let server_verified = gradient_entity::cached_path::Model {
            server_computed_hashes_and_sizes: true,
            ..returned_cached_path(IDEM_HASH)
        };
        let db = mock_db()
            .append_query_results([vec![legacy, server_verified.clone()]])
            .into_connection();

        let found = find_verified_cached_path_by_store_hash(
            &db,
            IDEM_HASH,
            verified(b"nar").nar_hash(),
            "",
            None,
        )
        .await
        .unwrap();
        assert_eq!(found.map(|row| row.id), Some(server_verified.id));
    }

    #[tokio::test]
    async fn find_verified_cached_path_by_store_hash_returns_none_when_verified_row_references_differ()
     {
        let server_verified = gradient_entity::cached_path::Model {
            server_computed_hashes_and_sizes: true,
            ..returned_cached_path(IDEM_HASH)
        };
        let db = mock_db()
            .append_query_results([vec![server_verified]])
            .into_connection();

        let found = find_verified_cached_path_by_store_hash(
            &db,
            IDEM_HASH,
            verified(b"nar").nar_hash(),
            "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-dep",
            None,
        )
        .await
        .unwrap();
        assert!(found.is_none());
    }
}
