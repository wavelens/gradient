/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use bytes::Bytes;
use futures::StreamExt as _;
use futures::stream::BoxStream;
use object_store::{ClientOptions, ObjectStore, ObjectStoreExt as _, PutPayload, path::Path};
pub use object_store::{MultipartUpload, WriteMultipart};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt as _};

/// Unified NAR file storage abstraction over local disk or an S3-compatible backend.
///
/// All NARs are stored pre-compressed (`.nar.zst`). The key path within the store is
/// `nars/{hash[..2]}/{hash[2..]}.nar.zst` (same two-level sharding used locally).
#[derive(Clone)]
pub struct NarStore {
    inner: Arc<dyn ObjectStore>,
    /// Non-empty only for S3: prepended to the object key.
    prefix: String,
    /// Set only for local storage; used by the orphan-file cleanup scan.
    local_base: Option<String>,
    /// S3 store - held separately to enable presigned URL generation via the
    /// [`object_store::signer::Signer`] trait.  `None` for local-disk stores.
    s3_signer: Option<Arc<object_store::aws::AmazonS3>>,
}

impl NarStore {
    /// Returns a clone of the underlying object store so callers (e.g. log storage)
    /// can share the same connection.
    pub fn inner(&self) -> Arc<dyn ObjectStore> {
        Arc::clone(&self.inner)
    }

    /// Returns the prefix used by this store (empty for local).
    pub fn prefix(&self) -> &str {
        &self.prefix
    }
}

impl NarStore {
    /// Create a local-disk-backed store rooted at `base_path`.
    pub fn local(base_path: &str) -> Result<Self> {
        std::fs::create_dir_all(base_path)
            .with_context(|| format!("Failed to create NAR storage directory: {}", base_path))?;
        let store = object_store::local::LocalFileSystem::new_with_prefix(base_path)
            .context("Failed to create local NAR storage")?;
        Ok(Self {
            inner: Arc::new(store),
            prefix: String::new(),
            local_base: Some(base_path.to_string()),
            s3_signer: None,
        })
    }

    /// Create an S3-backed store.
    ///
    /// `access_key_id` and `secret_access_key` are optional; when absent the AWS SDK
    /// falls back to instance profiles / environment variables.
    #[allow(
        clippy::too_many_arguments,
        reason = "arg-heavy; refactor tracked in #503"
    )]
    pub fn s3(
        bucket: &str,
        region: &str,
        endpoint: Option<&str>,
        access_key_id: Option<&str>,
        secret_access_key: Option<&str>,
        prefix: &str,
        virtual_hosted_style: bool,
    ) -> Result<Self> {
        let mut builder = object_store::aws::AmazonS3Builder::new()
            .with_bucket_name(bucket)
            .with_region(region)
            .with_client_options(
                ClientOptions::new().with_user_agent(
                    gradient_util::http::user_agent()
                        .parse()
                        .expect("static UA is valid"),
                ),
            );

        if let Some(ep) = endpoint {
            builder = builder
                .with_endpoint(ep)
                .with_virtual_hosted_style_request(virtual_hosted_style)
                .with_allow_http(true);
        }
        if let Some(key) = access_key_id {
            builder = builder.with_access_key_id(key);
        }
        if let Some(secret) = secret_access_key {
            builder = builder.with_secret_access_key(secret);
        }

        let store = builder.build().context("Failed to create S3 NAR storage")?;
        let store = Arc::new(store);

        Ok(Self {
            inner: Arc::clone(&store) as Arc<dyn ObjectStore>,
            prefix: crate::layout::normalize_prefix(prefix),
            local_base: None,
            s3_signer: Some(store),
        })
    }

    fn object_path(&self, hash: &str) -> Path {
        // Hash is validated at every callable entry point, but defend the
        // formatter anyway: a too-short hash would otherwise panic on
        // `&hash[..2]` / `&hash[2..]`.
        let (shard, stem) = if hash.len() >= 2 {
            (&hash[..2], &hash[2..])
        } else {
            ("__", hash)
        };
        Path::from(format!("{}nars/{}/{}.nar.zst", self.prefix, shard, stem,))
    }

    /// One canonical `HEAD`, mapping `NotFound` to `None`. Backs `exists`,
    /// `head_size` and the range-stream head probe.
    async fn head_object(&self, path: &Path) -> Result<Option<object_store::ObjectMeta>> {
        match self.inner.head(path).await {
            Ok(meta) => Ok(Some(meta)),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(e).context("Failed to head object"),
        }
    }

    async fn put_object(&self, path: Path, data: Vec<u8>) -> Result<()> {
        self.inner
            .put(&path, PutPayload::from(data))
            .await
            .context("Failed to upload object")?;
        Ok(())
    }

    async fn get_object(&self, path: &Path) -> Result<Option<Vec<u8>>> {
        match self.inner.get(path).await {
            Ok(result) => {
                let bytes = result
                    .bytes()
                    .await
                    .context("Failed to read object bytes")?;
                Ok(Some(bytes.to_vec()))
            }
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(e).context("Failed to get object"),
        }
    }

    async fn delete_object(&self, path: &Path) -> Result<()> {
        match self.inner.delete(path).await {
            Ok(_) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(e) => Err(e).context("Failed to delete object"),
        }
    }

    /// Streaming read. `offset == None` is a plain GET whose size comes from the
    /// object metadata; `offset == Some(o)` HEADs for the FULL size, returns an
    /// empty stream when `o` is at or past the end, else range-GETs from `o`.
    async fn get_stream_object(
        &self,
        path: &Path,
        offset: Option<u64>,
    ) -> Result<Option<(u64, BoxStream<'static, Result<Bytes>>)>> {
        use object_store::{GetOptions, GetRange};

        let Some(offset) = offset else {
            return match self.inner.get(path).await {
                Ok(result) => {
                    let size = result.meta.size;
                    let stream = result
                        .into_stream()
                        .map(|chunk| chunk.context("object stream chunk read failed"))
                        .boxed();
                    Ok(Some((size, stream)))
                }
                Err(object_store::Error::NotFound { .. }) => Ok(None),
                Err(e) => Err(e).context("Failed to open object stream"),
            };
        };

        let size = match self.head_object(path).await? {
            Some(meta) => meta.size,
            None => return Ok(None),
        };

        if offset >= size {
            let empty: BoxStream<'static, Result<Bytes>> = futures::stream::empty().boxed();
            return Ok(Some((size, empty)));
        }

        let opts = GetOptions {
            range: Some(GetRange::Offset(offset)),
            ..Default::default()
        };
        match self.inner.get_opts(path, opts).await {
            Ok(result) => {
                let stream = result
                    .into_stream()
                    .map(|chunk| chunk.context("object range stream chunk read failed"))
                    .boxed();
                Ok(Some((size, stream)))
            }
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(e).context("Failed to open object range stream"),
        }
    }

    async fn presign_object(
        &self,
        method: reqwest::Method,
        path: &Path,
        expires_in: std::time::Duration,
    ) -> Result<Option<String>> {
        use object_store::signer::Signer as _;

        let Some(signer) = &self.s3_signer else {
            return Ok(None);
        };

        let url = signer
            .signed_url(method, path, expires_in)
            .await
            .context("failed to generate presigned URL")?;

        Ok(Some(url.to_string()))
    }

    /// Verify the storage backend is reachable. Returns `Ok(())` when the
    /// underlying store responds (even with NotFound), or an error when the
    /// server cannot be reached at all (network error, 502, auth failure, …).
    pub async fn ping(&self) -> Result<()> {
        let probe = Path::from(format!("{}__gradient_ping__", self.prefix));
        match self.inner.head(&probe).await {
            Ok(_)
            | Err(object_store::Error::NotFound { .. })
            | Err(object_store::Error::PermissionDenied { .. }) => Ok(()),
            Err(e) => Err(e).context("Storage backend unreachable"),
        }
    }

    pub async fn put(&self, hash: &str, data: Vec<u8>) -> Result<()> {
        self.put_object(self.object_path(hash), data).await
    }

    /// Whether a NAR object for `hash` is already present (a single `HEAD`).
    /// Backs the idempotent-write guard so a re-push of identical content does
    /// not rewrite the object - on a versioning-enabled bucket every rewrite is
    /// a retained version that no S3-API GC can reclaim.
    pub async fn exists(&self, hash: &str) -> Result<bool> {
        Ok(self.head_object(&self.object_path(hash)).await?.is_some())
    }

    /// Size in bytes of the stored NAR object for `hash`, or `None` when
    /// absent. Backs the presigned-upload commit check: the worker PUT the
    /// bytes directly to object storage, so this HEAD is the only server-side
    /// evidence the object actually landed with the reported size.
    pub async fn head_size(&self, hash: &str) -> Result<Option<u64>> {
        Ok(self
            .head_object(&self.object_path(hash))
            .await?
            .map(|meta| meta.size))
    }

    /// Verify a stored NAR object against its reported file_hash and size.
    /// Always HEADs to confirm existence and size; when `rehash` is set it
    /// additionally GETs the object and recomputes the file hash (authoritative
    /// but costs a full object read).
    pub async fn verify(
        &self,
        hash: &str,
        expected_file_hash: &str,
        expected_size: u64,
        rehash: bool,
    ) -> std::result::Result<(), crate::digest::VerifyError> {
        match self.head_size(hash).await? {
            Some(size) if size == expected_size => {}
            Some(size) => {
                return Err(crate::digest::VerifyError::Size {
                    expected: expected_size,
                    actual: size,
                });
            }
            None => return Err(crate::digest::VerifyError::Missing),
        }
        if rehash {
            let bytes = self
                .get(hash)
                .await?
                .ok_or(crate::digest::VerifyError::Missing)?;
            crate::digest::verify_nar_bytes(&bytes, expected_file_hash, expected_size)?;
        }

        Ok(())
    }

    /// Initiate a multipart upload for the NAR identified by `hash`.
    ///
    /// Returns a [`WriteMultipart`] configured with `chunk_size`-byte parts.
    /// The caller writes compressed data into it, then calls `.finish().await`.
    pub async fn put_streaming(&self, hash: &str, chunk_size: usize) -> Result<WriteMultipart> {
        let upload = self
            .inner
            .put_multipart(&self.object_path(hash))
            .await
            .context("Failed to initiate multipart upload")?;
        Ok(WriteMultipart::new_with_chunk_size(upload, chunk_size))
    }

    /// Stream `reader` into object storage under `hash` via a multipart upload,
    /// so a large NAR is never held whole in memory: bytes flow reader -> part
    /// buffer -> object store with at most `MAX_INFLIGHT_PARTS` uploads pending.
    pub async fn put_reader<R: AsyncRead + Unpin + Send>(
        &self,
        hash: &str,
        mut reader: R,
    ) -> Result<()> {
        const PART_SIZE: usize = 8 * 1024 * 1024;
        const MAX_INFLIGHT_PARTS: usize = 2;
        const READ_CHUNK: usize = 256 * 1024;

        let mut upload = self.put_streaming(hash, PART_SIZE).await?;
        let mut buf = vec![0u8; READ_CHUNK];
        loop {
            let n = reader
                .read(&mut buf)
                .await
                .context("read NAR while streaming to storage")?;
            if n == 0 {
                break;
            }
            upload.write(&buf[..n]);
            upload
                .wait_for_capacity(MAX_INFLIGHT_PARTS)
                .await
                .context("multipart upload backpressure")?;
        }
        upload
            .finish()
            .await
            .context("finish multipart NAR upload")?;
        Ok(())
    }

    pub async fn get(&self, hash: &str) -> Result<Option<Vec<u8>>> {
        self.get_object(&self.object_path(hash)).await
    }

    /// Streaming counterpart to [`Self::get`].
    ///
    /// Returns the object's `(size, byte_stream)` pair without buffering the
    /// whole NAR in memory. Used by the WebSocket NAR-serving path so a 200 MB
    /// `gcc-lib` doesn't pin the entire file in RAM before the first
    /// `NarPush` chunk goes out.
    pub async fn get_stream(
        &self,
        hash: &str,
    ) -> Result<Option<(u64, BoxStream<'static, Result<Bytes>>)>> {
        self.get_stream_object(&self.object_path(hash), None).await
    }

    /// Range variant of [`Self::get_stream`]: streams the stored object from
    /// `offset` to the end. The returned size is the FULL object size (so the
    /// caller can compare it against a worker's reported `received_bytes`).
    /// `offset == 0` is equivalent to [`Self::get_stream`]; an `offset` at or
    /// past the end yields an empty stream with the real size.
    pub async fn get_stream_from(
        &self,
        hash: &str,
        offset: u64,
    ) -> Result<Option<(u64, BoxStream<'static, Result<Bytes>>)>> {
        self.get_stream_object(&self.object_path(hash), Some(offset))
            .await
    }

    pub async fn delete(&self, hash: &str) -> Result<()> {
        self.delete_object(&self.object_path(hash)).await
    }

    /// Returns the local base path when using local-disk storage; `None` for S3.
    pub fn local_base(&self) -> Option<&str> {
        self.local_base.as_deref()
    }

    /// Generate a presigned GET URL valid for `expires_in` for the NAR
    /// identified by `hash`.
    ///
    /// Returns `None` for local-disk stores. Returns `Some(url_string)` for
    /// S3-backed stores so workers can download directly without routing data
    /// through the Gradient server.
    pub async fn presigned_get_url(
        &self,
        hash: &str,
        expires_in: std::time::Duration,
    ) -> Result<Option<String>> {
        self.presign_object(reqwest::Method::GET, &self.object_path(hash), expires_in)
            .await
    }

    /// Generate a presigned PUT URL valid for `expires_in` for the NAR
    /// identified by `hash`.
    ///
    /// Returns `None` for local-disk stores (no presigning needed - the server
    /// accepts direct `NarPush` WebSocket frames).  Returns `Some(url_string)`
    /// for S3-backed stores so workers can upload directly to S3 without
    /// routing all NAR data through the Gradient server.
    pub async fn presigned_put_url(
        &self,
        hash: &str,
        expires_in: std::time::Duration,
    ) -> Result<Option<String>> {
        self.presign_object(reqwest::Method::PUT, &self.object_path(hash), expires_in)
            .await
    }

    /// Object-store path for a fleet-shared eval-cache blob keyed by flake
    /// fingerprint. Namespaced under `eval-cache/` so it never collides with the
    /// `nars/` NAR layout (#386).
    fn eval_cache_path(&self, fingerprint: &str) -> Path {
        Path::from(format!("{}eval-cache/{}", self.prefix, fingerprint))
    }

    pub async fn put_eval_cache(&self, fingerprint: &str, data: Vec<u8>) -> Result<()> {
        self.put_object(self.eval_cache_path(fingerprint), data)
            .await
    }

    pub async fn get_eval_cache(&self, fingerprint: &str) -> Result<Option<Vec<u8>>> {
        self.get_object(&self.eval_cache_path(fingerprint)).await
    }

    /// Streaming counterpart to [`Self::get_eval_cache`]; mirrors
    /// [`Self::get_stream`] so the inline pull path never buffers the whole blob.
    pub async fn get_eval_cache_stream(
        &self,
        fingerprint: &str,
    ) -> Result<Option<(u64, BoxStream<'static, Result<Bytes>>)>> {
        self.get_stream_object(&self.eval_cache_path(fingerprint), None)
            .await
    }

    /// Presigned GET URL for an eval-cache blob; `None` for local-disk stores.
    pub async fn presigned_eval_cache_get_url(
        &self,
        fingerprint: &str,
        expires_in: std::time::Duration,
    ) -> Result<Option<String>> {
        self.presign_object(
            reqwest::Method::GET,
            &self.eval_cache_path(fingerprint),
            expires_in,
        )
        .await
    }

    /// Presigned PUT URL for an eval-cache blob; `None` for local-disk stores.
    pub async fn presigned_eval_cache_put_url(
        &self,
        fingerprint: &str,
        expires_in: std::time::Duration,
    ) -> Result<Option<String>> {
        self.presign_object(
            reqwest::Method::PUT,
            &self.eval_cache_path(fingerprint),
            expires_in,
        )
        .await
    }

    pub async fn delete_eval_cache(&self, fingerprint: &str) -> Result<()> {
        self.delete_object(&self.eval_cache_path(fingerprint)).await
    }

    /// Object-store path for a build-request blob keyed by org + BLAKE3 hash.
    /// Layout: `<prefix>build-request-blobs/<org-uuid>/<hh>/<full-hex>`.
    fn blob_path(&self, org: uuid::Uuid, hash: &[u8; 32]) -> Path {
        let hex = hex::encode(hash);
        let shard = &hex[..2];
        Path::from(format!(
            "{}build-request-blobs/{}/{}/{}",
            self.prefix, org, shard, hex,
        ))
    }

    pub async fn put_blob(&self, org: uuid::Uuid, hash: &[u8; 32], data: Vec<u8>) -> Result<()> {
        self.put_object(self.blob_path(org, hash), data).await
    }

    pub async fn get_blob(&self, org: uuid::Uuid, hash: &[u8; 32]) -> Result<Option<Vec<u8>>> {
        self.get_object(&self.blob_path(org, hash)).await
    }

    pub async fn delete_blob(&self, org: uuid::Uuid, hash: &[u8; 32]) -> Result<()> {
        self.delete_object(&self.blob_path(org, hash)).await
    }

    /// Lists all NAR hashes currently present in the store (both local and S3).
    /// Returns the full hash strings as stored (e.g. `"ab12cd34..."`).
    pub async fn list_hashes(&self) -> Result<Vec<String>> {
        Ok(self
            .list_hashes_with_modified()
            .await?
            .into_iter()
            .map(|(hash, _)| hash)
            .collect())
    }

    /// Like [`Self::list_hashes`] but pairs each hash with its object's
    /// last-modified time as a unix timestamp (seconds). The orphan-file sweep
    /// uses this to spare freshly-written NARs: an upload lands on disk before
    /// the eval commits its `derivation`/`cached_path` rows, so for a brief
    /// window the keep-set does not yet reference it - reclaiming it then strands
    /// a zombie `cached_path` the dispatch gate trusts.
    pub async fn list_hashes_with_modified(&self) -> Result<Vec<(String, i64)>> {
        let prefix = Path::from(format!("{}nars", self.prefix));
        let mut stream = self.inner.list(Some(&prefix));
        let mut hashes = Vec::new();
        while let Some(item) = stream.next().await {
            let meta = item.context("Failed to list NAR store")?;
            // Path format: `{prefix}nars/{first2}/{rest}.nar.zst`
            let p = meta.location.to_string();
            if let Some(name) = p.split('/').next_back()
                && let Some(stem) = name.strip_suffix(".nar.zst")
            {
                // Reconstruct full hash from parent dir + stem.
                let parts: Vec<&str> = p.split('/').collect();
                if parts.len() >= 2 {
                    let dir = parts[parts.len() - 2];
                    hashes.push((format!("{}{}", dir, stem), meta.last_modified.timestamp()));
                }
            }
        }
        Ok(hashes)
    }

    /// Lists every build-request blob currently in storage. Returns
    /// `(org, hash)` pairs reconstructed from the
    /// `build-request-blobs/<org-uuid>/<shard>/<full-hex>` path layout. Entries
    /// whose name does not match the layout are skipped.
    pub async fn list_blobs(&self) -> Result<Vec<(uuid::Uuid, [u8; 32])>> {
        let prefix = Path::from(format!("{}build-request-blobs", self.prefix));
        let mut stream = self.inner.list(Some(&prefix));
        let mut out = Vec::new();
        while let Some(item) = stream.next().await {
            let meta = item.context("Failed to list build-request-blobs")?;
            let p = meta.location.to_string();
            let parts: Vec<&str> = p.split('/').collect();
            if parts.len() < 3 {
                continue;
            }
            let hash_hex = parts[parts.len() - 1];
            let org_part = parts[parts.len() - 3];
            let Ok(org) = uuid::Uuid::parse_str(org_part) else {
                tracing::debug!(path = %p, "skipping blob with non-uuid org dir");
                continue;
            };
            let Ok(hash_vec) = hex::decode(hash_hex) else {
                tracing::debug!(path = %p, "skipping blob with non-hex name");
                continue;
            };
            if hash_vec.len() != 32 {
                tracing::debug!(path = %p, "skipping blob with non-32-byte hash");
                continue;
            }
            let mut hash = [0u8; 32];
            hash.copy_from_slice(&hash_vec);
            out.push((org, hash));
        }
        Ok(out)
    }
}

impl std::fmt::Debug for NarStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let backend = if self.local_base.is_some() {
            "local"
        } else {
            "s3"
        };
        f.debug_struct("NarStore")
            .field("backend", &backend)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn local_store() -> (TempDir, NarStore) {
        let dir = TempDir::new().expect("tempdir");
        let store = NarStore::local(dir.path().to_str().unwrap()).expect("local store");
        (dir, store)
    }

    #[tokio::test]
    async fn get_stream_returns_full_payload_in_order() {
        let (_d, store) = local_store();
        // Use a 9 MiB payload so it crosses the local-FS read boundary and
        // typically arrives in multiple chunks.
        let mut payload = Vec::with_capacity(9 * 1024 * 1024);
        for i in 0..(9 * 1024 * 1024 / 4) {
            payload.extend_from_slice(&(i as u32).to_le_bytes());
        }
        store.put("abc123", payload.clone()).await.expect("put");

        let (size, mut stream) = store
            .get_stream("abc123")
            .await
            .expect("get_stream")
            .expect("Some");
        assert_eq!(size as usize, payload.len());

        let mut assembled = Vec::with_capacity(payload.len());
        while let Some(chunk) = stream.next().await {
            assembled.extend_from_slice(&chunk.expect("chunk"));
        }
        assert_eq!(assembled, payload);
    }

    #[tokio::test]
    async fn put_reader_round_trips_multipart_payload() {
        let (_d, store) = local_store();
        // Larger than the 8 MiB part size so the multipart path spans >1 part.
        let mut payload = Vec::with_capacity(20 * 1024 * 1024);
        for i in 0..(20 * 1024 * 1024 / 4) {
            payload.extend_from_slice(&(i as u32).to_le_bytes());
        }
        store
            .put_reader("streamed", &payload[..])
            .await
            .expect("put_reader");
        let got = store.get("streamed").await.expect("get").expect("present");
        assert_eq!(got, payload);
    }

    #[tokio::test]
    async fn exists_reflects_presence() {
        let (_d, store) = local_store();
        assert!(!store.exists("ab12cd").await.expect("head"));
        store.put("ab12cd", b"data".to_vec()).await.expect("put");
        assert!(store.exists("ab12cd").await.expect("head"));
    }

    #[tokio::test]
    async fn head_size_none_for_missing() {
        let (_d, store) = local_store();
        assert_eq!(store.head_size("ab12cd").await.expect("head"), None);
    }

    #[tokio::test]
    async fn head_size_reports_object_len() {
        let (_d, store) = local_store();
        store.put("ab12cd", vec![7u8; 1234]).await.expect("put");
        assert_eq!(store.head_size("ab12cd").await.expect("head"), Some(1234));
    }

    #[tokio::test]
    async fn get_stream_returns_none_for_missing() {
        let (_d, store) = local_store();
        let r = store.get_stream("does-not-exist").await.expect("Ok");
        assert!(r.is_none());
    }

    #[tokio::test]
    async fn verify_ok_on_size_match_without_rehash() {
        let (_d, store) = local_store();
        let bytes = b"verify me".to_vec();
        let file_hash = crate::digest::file_hash_sri(&bytes);
        store.put("ab12cd", bytes.clone()).await.expect("put");
        assert!(
            store
                .verify("ab12cd", &file_hash, bytes.len() as u64, false)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn verify_size_mismatch_is_size_error() {
        let (_d, store) = local_store();
        store.put("ab12cd", vec![7u8; 100]).await.expect("put");
        let err = store
            .verify("ab12cd", "sha256-", 99, false)
            .await
            .unwrap_err();
        assert!(matches!(err, crate::digest::VerifyError::Size { .. }));
    }

    #[tokio::test]
    async fn verify_missing_object_is_missing_error() {
        let (_d, store) = local_store();
        let err = store
            .verify("ab12cd", "sha256-", 10, false)
            .await
            .unwrap_err();
        assert!(matches!(err, crate::digest::VerifyError::Missing));
    }

    #[tokio::test]
    async fn verify_rehash_ok_on_content_match() {
        let (_d, store) = local_store();
        let bytes = b"rehash content".to_vec();
        let file_hash = crate::digest::file_hash_sri(&bytes);
        store.put("ab12cd", bytes.clone()).await.expect("put");
        assert!(
            store
                .verify("ab12cd", &file_hash, bytes.len() as u64, true)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn verify_rehash_tampered_bytes_is_hash_error() {
        let (_d, store) = local_store();
        let good = b"rehash content".to_vec();
        let file_hash = crate::digest::file_hash_sri(&good);
        let mut stored = good.clone();
        *stored.last_mut().unwrap() ^= 0xff;
        store.put("ab12cd", stored.clone()).await.expect("put");
        let err = store
            .verify("ab12cd", &file_hash, stored.len() as u64, true)
            .await
            .unwrap_err();
        assert!(matches!(err, crate::digest::VerifyError::Hash { .. }));
    }

    #[tokio::test]
    async fn get_stream_from_returns_suffix() {
        let (_d, store) = local_store();
        let payload: Vec<u8> = (0u32..1024).flat_map(|i| i.to_le_bytes()).collect();
        store.put("def456", payload.clone()).await.expect("put");

        let (size, mut stream) = store
            .get_stream_from("def456", 100)
            .await
            .expect("get_stream_from")
            .expect("Some");
        assert_eq!(size as usize, payload.len(), "size is the FULL object size");

        let mut assembled = Vec::new();
        while let Some(chunk) = stream.next().await {
            assembled.extend_from_slice(&chunk.expect("chunk"));
        }
        assert_eq!(assembled, payload[100..]);
    }

    #[tokio::test]
    async fn get_stream_from_zero_equals_full() {
        let (_d, store) = local_store();
        store.put("ghi789", b"abcdef".to_vec()).await.unwrap();
        let (_size, mut stream) = store.get_stream_from("ghi789", 0).await.unwrap().unwrap();
        let mut buf = Vec::new();
        while let Some(c) = stream.next().await {
            buf.extend_from_slice(&c.unwrap());
        }
        assert_eq!(buf, b"abcdef");
    }

    #[tokio::test]
    async fn get_stream_from_past_end_is_empty() {
        let (_d, store) = local_store();
        store.put("jkl012", b"abc".to_vec()).await.unwrap();
        let (size, mut stream) = store.get_stream_from("jkl012", 99).await.unwrap().unwrap();
        assert_eq!(size, 3);
        assert!(stream.next().await.is_none());
    }
}
