/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use object_store::{ObjectStore, PutPayload, path::Path};
use std::sync::Arc;

/// Unified NAR file storage abstraction over local disk or an S3-compatible backend.
///
/// All NARs are stored pre-compressed (`.nar.zst`). The key path within the store is
/// `nars/{hash[..2]}/{hash[2..]}.nar.zst` (same two-level sharding used locally).
pub struct NarStore {
    inner: Arc<dyn ObjectStore>,
    /// Non-empty only for S3: prepended to the object key.
    prefix: String,
    /// Set only for local storage; used by the orphan-file cleanup scan.
    local_base: Option<String>,
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
        })
    }

    /// Create an S3-backed store.
    ///
    /// `access_key_id` and `secret_access_key` are optional; when absent the AWS SDK
    /// falls back to instance profiles / environment variables.
    pub fn s3(
        bucket: &str,
        region: &str,
        endpoint: Option<&str>,
        access_key_id: Option<&str>,
        secret_access_key: Option<&str>,
        prefix: &str,
    ) -> Result<Self> {
        let mut builder = object_store::aws::AmazonS3Builder::new()
            .with_bucket_name(bucket)
            .with_region(region);

        if let Some(ep) = endpoint {
            builder = builder
                .with_endpoint(ep)
                .with_virtual_hosted_style_request(false)
                .with_allow_http(true);
        }
        if let Some(key) = access_key_id {
            builder = builder.with_access_key_id(key);
        }
        if let Some(secret) = secret_access_key {
            builder = builder.with_secret_access_key(secret);
        }

        let store = builder.build().context("Failed to create S3 NAR storage")?;

        let normalized_prefix = if prefix.is_empty() || prefix.ends_with('/') {
            prefix.to_string()
        } else {
            format!("{}/", prefix)
        };

        Ok(Self {
            inner: Arc::new(store),
            prefix: normalized_prefix,
            local_base: None,
        })
    }

    fn object_path(&self, hash: &str) -> Path {
        Path::from(format!(
            "{}nars/{}/{}.nar.zst",
            self.prefix,
            &hash[..2],
            &hash[2..]
        ))
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

    pub async fn delete(&self, hash: &str) -> Result<()> {
        match self.inner.delete(&self.object_path(hash)).await {
            Ok(_) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(e) => Err(e).context("Failed to delete NAR"),
        }
    }

    /// Returns the local base path when using local-disk storage; `None` for S3.
    /// Used by the orphan-file cleanup scan which requires directory listing.
    pub fn local_base(&self) -> Option<&str> {
        self.local_base.as_deref()
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
