/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use bytes::Bytes;
use futures::StreamExt as _;
use futures::stream::BoxStream;
pub use object_store::{MultipartUpload, WriteMultipart};
use object_store::{ClientOptions, ObjectStore, ObjectStoreExt as _, PutPayload, path::Path};
use std::sync::Arc;

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
    #[allow(clippy::too_many_arguments)]
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
            .with_client_options(ClientOptions::new().with_user_agent(
                gradient_util::http::user_agent().parse().expect("static UA is valid"),
            ));

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

        let normalized_prefix = if prefix.is_empty() || prefix.ends_with('/') {
            prefix.to_string()
        } else {
            format!("{}/", prefix)
        };

        Ok(Self {
            inner: Arc::clone(&store) as Arc<dyn ObjectStore>,
            prefix: normalized_prefix,
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
        self.inner
            .put(&self.object_path(hash), PutPayload::from(data))
            .await
            .context("Failed to upload NAR")?;
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

    pub async fn get(&self, hash: &str) -> Result<Option<Vec<u8>>> {
        match self.inner.get(&self.object_path(hash)).await {
            Ok(result) => {
                let bytes = result.bytes().await.context("Failed to read NAR bytes")?;
                Ok(Some(bytes.to_vec()))
            }
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(e).context("Failed to get NAR"),
        }
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
        match self.inner.get(&self.object_path(hash)).await {
            Ok(result) => {
                let size = result.meta.size;
                let stream = result
                    .into_stream()
                    .map(|chunk| chunk.context("NAR stream chunk read failed"))
                    .boxed();
                Ok(Some((size, stream)))
            }
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(e).context("Failed to open NAR stream"),
        }
    }

    pub async fn delete(&self, hash: &str) -> Result<()> {
        match self.inner.delete(&self.object_path(hash)).await {
            Ok(_) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(e) => Err(e).context("Failed to delete NAR"),
        }
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
        use object_store::signer::Signer as _;

        let signer = match &self.s3_signer {
            Some(s) => s,
            None => return Ok(None),
        };

        let path = self.object_path(hash);
        let url = signer
            .signed_url(reqwest::Method::GET, &path, expires_in)
            .await
            .context("failed to generate presigned GET URL")?;

        Ok(Some(url.to_string()))
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
        use object_store::signer::Signer as _;

        let signer = match &self.s3_signer {
            Some(s) => s,
            None => return Ok(None),
        };

        let path = self.object_path(hash);
        let url = signer
            .signed_url(reqwest::Method::PUT, &path, expires_in)
            .await
            .context("failed to generate presigned PUT URL")?;

        Ok(Some(url.to_string()))
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
        self.inner
            .put(&self.blob_path(org, hash), PutPayload::from(data))
            .await
            .context("Failed to upload build-request blob")?;
        Ok(())
    }

    pub async fn get_blob(&self, org: uuid::Uuid, hash: &[u8; 32]) -> Result<Option<Vec<u8>>> {
        match self.inner.get(&self.blob_path(org, hash)).await {
            Ok(result) => Ok(Some(
                result
                    .bytes()
                    .await
                    .context("Failed to read build-request blob bytes")?
                    .to_vec(),
            )),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(e).context("Failed to get build-request blob"),
        }
    }

    pub async fn delete_blob(&self, org: uuid::Uuid, hash: &[u8; 32]) -> Result<()> {
        match self.inner.delete(&self.blob_path(org, hash)).await {
            Ok(_) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(e) => Err(e).context("Failed to delete build-request blob"),
        }
    }

    /// Lists all NAR hashes currently present in the store (both local and S3).
    /// Returns the full hash strings as stored (e.g. `"ab12cd34..."`).
    pub async fn list_hashes(&self) -> Result<Vec<String>> {
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
                    hashes.push(format!("{}{}", dir, stem));
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
    async fn get_stream_returns_none_for_missing() {
        let (_d, store) = local_store();
        let r = store.get_stream("does-not-exist").await.expect("Ok");
        assert!(r.is_none());
    }
}
