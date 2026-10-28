/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::HotNarCache;
use crate::admission::ObjectKey;
use crate::eval_cache_segment::eval_cache_segment_prefix;
use crate::timed::{OpGuard, TimedStore, watch_stream};
use anyhow::{Context, Result};
use bytes::{Bytes, BytesMut};
use futures::StreamExt as _;
use futures::stream::BoxStream;
use gradient_types::ids::CacheId;
use gradient_util::telemetry::{GAUGES, STATS};
use gradient_wire::constants::{BULK_CHUNK_SIZE, MULTIPART_NAR_BYTES, PRESIGN_TTL};
use object_store::{ClientOptions, ObjectStore, ObjectStoreExt as _, PutPayload, path::Path};
pub use object_store::{MultipartUpload, WriteMultipart};
use std::io::SeekFrom;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncSeekExt as _};

#[derive(Clone, Copy, Debug)]
pub struct S3Timeouts {
    pub read_timeout: std::time::Duration,
    pub max_retries: usize,
    pub retry_timeout: std::time::Duration,
}

impl Default for S3Timeouts {
    fn default() -> Self {
        Self {
            read_timeout: std::time::Duration::from_secs(60),
            max_retries: 3,
            retry_timeout: std::time::Duration::from_secs(250),
        }
    }
}

const WRITE_STALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

#[derive(Debug)]
pub(crate) struct SharedHttpConnector {
    pub(crate) read_timeout: std::time::Duration,
    pub(crate) allow_http: bool,
}

impl object_store::client::HttpConnector for SharedHttpConnector {
    fn connect(&self, _: &ClientOptions) -> object_store::Result<object_store::client::HttpClient> {
        let client = gradient_util::http::untimed_client_builder()
            .read_timeout(self.read_timeout)
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .https_only(!self.allow_http)
            .build()
            .map_err(|e| object_store::Error::Generic {
                store: "S3",
                source: Box::new(e),
            })?;

        Ok(object_store::client::HttpClient::new(client))
    }
}

#[derive(Clone)]
pub struct NarStore {
    inner: Arc<dyn ObjectStore>,
    prefix: String,
    local_base: Option<PathBuf>,
    s3_signer: Option<Arc<object_store::aws::AmazonS3>>,
    hot: Arc<HotNarCache>,
}

pub(crate) const MIN_WRITE_THROUGHPUT_BYTES_PER_SEC: u64 = 256 * 1024;

pub(crate) fn single_write_budget(len: usize) -> std::time::Duration {
    WRITE_STALL_TIMEOUT
        + std::time::Duration::from_secs(len as u64 / MIN_WRITE_THROUGHPUT_BYTES_PER_SEC)
}

pub(crate) async fn bounded<T, E>(
    op: impl std::future::Future<Output = std::result::Result<T, E>>,
    budget: std::time::Duration,
    what: &'static str,
) -> Result<T>
where
    E: std::error::Error + Send + Sync + 'static,
{
    match tokio::time::timeout(budget, op).await {
        Ok(r) => r.context(what),
        Err(_) => anyhow::bail!("{what} made no progress within {}s", budget.as_secs()),
    }
}

impl NarStore {
    pub fn inner(&self) -> Arc<dyn ObjectStore> {
        Arc::clone(&self.inner)
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }
}

#[cfg(test)]
impl NarStore {
    pub(crate) fn over(inner: Arc<dyn ObjectStore>) -> Self {
        Self {
            inner,
            prefix: String::new(),
            local_base: None,
            s3_signer: None,
            hot: Arc::new(HotNarCache::disabled()),
        }
    }
}

impl NarStore {
    pub fn local(base_path: &std::path::Path) -> Result<Self> {
        std::fs::create_dir_all(base_path)
            .with_context(|| format!("create NAR storage directory: {}", base_path.display()))?;
        let store = object_store::local::LocalFileSystem::new_with_prefix(base_path)
            .context("create local NAR storage")?;
        Ok(Self {
            inner: Arc::new(TimedStore::new(Arc::new(store), &STATS, &GAUGES)),
            prefix: String::new(),
            local_base: Some(base_path.to_path_buf()),
            s3_signer: None,
            hot: Arc::new(HotNarCache::disabled()),
        })
    }

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
        timeouts: S3Timeouts,
    ) -> Result<Self> {
        let retry_config = object_store::RetryConfig {
            max_retries: timeouts.max_retries,
            retry_timeout: timeouts.retry_timeout,
            ..Default::default()
        };

        // The object_store default total timeout of 30s was cancelling every larger NAR and
        // retrying it into a hard failure. The read timeout is policing progress instead, reset on
        // every chunk.
        let mut builder = object_store::aws::AmazonS3Builder::new()
            .with_bucket_name(bucket)
            .with_region(region)
            .with_retry(retry_config)
            .with_http_connector(SharedHttpConnector {
                read_timeout: timeouts.read_timeout,
                allow_http: endpoint.is_some(),
            });

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

        let store = builder.build().context("create S3 NAR storage")?;
        let store = Arc::new(store);

        Ok(Self {
            inner: Arc::new(TimedStore::new(
                Arc::clone(&store) as Arc<dyn ObjectStore>,
                &STATS,
                &GAUGES,
            )),
            prefix: crate::layout::normalize_prefix(prefix),
            local_base: None,
            s3_signer: Some(store),
            hot: Arc::new(HotNarCache::disabled()),
        })
    }

    fn object_path(&self, hash: &str) -> Path {
        let (shard, stem) = if hash.len() >= 2 {
            (&hash[..2], &hash[2..])
        } else {
            ("__", hash)
        };
        Path::from(format!("{}nars/{}/{}.nar.zst", self.prefix, shard, stem,))
    }

    async fn head_object(&self, path: &Path) -> Result<Option<object_store::ObjectMeta>> {
        match self.inner.head(path).await {
            Ok(meta) => Ok(Some(meta)),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(e).context("head object"),
        }
    }

    async fn put_object(&self, path: Path, data: Vec<u8>) -> Result<()> {
        let budget = single_write_budget(data.len());
        bounded(
            self.inner.put(&path, PutPayload::from(data)),
            budget,
            "Failed to upload object",
        )
        .await?;
        Ok(())
    }

    async fn get_object(&self, path: &Path) -> Result<Option<Vec<u8>>> {
        match self.inner.get(path).await {
            Ok(result) => {
                let bytes = result.bytes().await.context("read object bytes")?;
                Ok(Some(bytes.to_vec()))
            }
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(e).context("get object"),
        }
    }

    async fn delete_object(&self, path: &Path) -> Result<()> {
        match self.inner.delete(path).await {
            Ok(_) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(e) => Err(e).context("delete object"),
        }
    }

    async fn get_stream_object(
        &self,
        path: &Path,
        offset: Option<u64>,
    ) -> Result<Option<(u64, BoxStream<'static, Result<Bytes>>)>> {
        use object_store::{GetOptions, GetRange};

        if let Some(base) = &self.local_base {
            let guard = OpGuard::start(&STATS, "get");
            let opened = local_stream(base.join(path.as_ref()), offset.unwrap_or(0)).await;

            guard.finish_with(opened.is_ok());
            return opened
                .map(|found| found.map(|(size, s)| (size, watch_stream(s, &STATS, &GAUGES))));
        }

        let Some(offset) = offset else {
            return match self.inner.get(path).await {
                Ok(result) => {
                    let size = result.meta.size;
                    let stream = result
                        .into_stream()
                        .map(|chunk| chunk.context("read the object stream chunk"))
                        .boxed();
                    Ok(Some((size, stream)))
                }
                Err(object_store::Error::NotFound { .. }) => Ok(None),
                Err(e) => Err(e).context("open object stream"),
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
                    .map(|chunk| chunk.context("read the object range stream chunk"))
                    .boxed();
                Ok(Some((size, stream)))
            }
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(e).context("open object range stream"),
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
            .context("generate presigned URL")?;

        Ok(Some(url.to_string()))
    }

    pub async fn ping(&self) -> Result<()> {
        let probe = Path::from(format!("{}__gradient_ping__", self.prefix));
        match self.inner.head(&probe).await {
            Ok(_)
            | Err(object_store::Error::NotFound { .. })
            | Err(object_store::Error::PermissionDenied { .. }) => Ok(()),
            Err(e) => Err(e).context("storage backend unreachable"),
        }
    }

    pub async fn put(&self, hash: &str, data: Vec<u8>) -> Result<()> {
        self.put_object(self.object_path(hash), data).await
    }

    pub async fn exists(&self, hash: &str) -> Result<bool> {
        Ok(self.head_object(&self.object_path(hash)).await?.is_some())
    }

    pub async fn head_size(&self, hash: &str) -> Result<Option<u64>> {
        Ok(self
            .head_object(&self.object_path(hash))
            .await?
            .map(|meta| meta.size))
    }

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

    pub async fn put_reader<R: AsyncRead + Unpin + Send>(
        &self,
        hash: &str,
        reader: R,
    ) -> Result<()> {
        self.put_reader_at(self.object_path(hash), reader).await
    }

    async fn put_reader_at<R: AsyncRead + Unpin + Send>(
        &self,
        object: Path,
        mut reader: R,
    ) -> Result<()> {
        const PART_SIZE: usize = 8 * 1024 * 1024;
        const MAX_INFLIGHT_PARTS: usize = 2;
        const READ_CHUNK: usize = 256 * 1024;

        let multipart = self
            .inner
            .put_multipart(&object)
            .await
            .context("initiate multipart upload")?;
        let mut upload = WriteMultipart::new_with_chunk_size(multipart, PART_SIZE);
        let mut buf = vec![0u8; READ_CHUNK];
        loop {
            let n = reader
                .read(&mut buf)
                .await
                .context("read the object while streaming to storage")?;
            if n == 0 {
                break;
            }
            upload.write(&buf[..n]);
            bounded(
                upload.wait_for_capacity(MAX_INFLIGHT_PARTS),
                WRITE_STALL_TIMEOUT,
                "multipart upload backpressure",
            )
            .await?;
        }
        bounded(
            upload.finish(),
            WRITE_STALL_TIMEOUT,
            "finish multipart upload",
        )
        .await?;
        Ok(())
    }

    pub async fn get(&self, hash: &str) -> Result<Option<Vec<u8>>> {
        self.get_object(&self.object_path(hash)).await
    }

    pub async fn adopt_file(&self, hash: &str, path: &std::path::Path) -> Result<()> {
        self.adopt_into(self.object_path(hash), path).await
    }

    async fn adopt_into(&self, object: Path, staged: &std::path::Path) -> Result<()> {
        if let Some(base) = &self.local_base {
            let dest = base.join(object.as_ref());
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .context("create the object directory")?;
            }
            match tokio::fs::rename(staged, &dest).await {
                Ok(()) => return Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {}
                Err(e) => return Err(e).context("rename the staged file into the store"),
            }
        }

        let file = tokio::fs::File::open(staged)
            .await
            .context("open the staged file")?;
        self.put_reader_at(object, file).await?;
        tokio::fs::remove_file(staged)
            .await
            .context("remove the staged file after upload")
    }

    pub async fn get_stream(
        &self,
        hash: &str,
    ) -> Result<Option<(u64, BoxStream<'static, Result<Bytes>>)>> {
        self.get_stream_object(&self.object_path(hash), None).await
    }

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

    pub fn local_base(&self) -> Option<&std::path::Path> {
        self.local_base.as_deref()
    }

    pub fn presigner_available(&self) -> bool {
        self.s3_signer.is_some()
    }

    pub fn with_hot_cache(mut self, hot: HotNarCache) -> Self {
        self.hot = Arc::new(hot);
        self
    }

    pub fn hot(&self) -> &HotNarCache {
        &self.hot
    }

    pub async fn presigned_get_url(
        &self,
        hash: &str,
        expires_in: std::time::Duration,
    ) -> Result<Option<String>> {
        self.presign_object(reqwest::Method::GET, &self.object_path(hash), expires_in)
            .await
    }

    pub async fn presigned_put_url(
        &self,
        hash: &str,
        expires_in: std::time::Duration,
    ) -> Result<Option<String>> {
        self.presign_object(reqwest::Method::PUT, &self.object_path(hash), expires_in)
            .await
    }

    pub async fn presigned_multipart(
        &self,
        hash: &str,
        nar_size: u64,
    ) -> Result<Option<gradient_wire::types::PresignedMultipart>> {
        let Some(s3) = &self.s3_signer else {
            return Ok(None);
        };
        crate::multipart::presign(s3, &STATS, &self.object_path(hash), nar_size)
            .await
            .map(Some)
    }

    pub async fn complete_multipart(
        &self,
        hash: &str,
        receipt: &gradient_wire::types::CompletedMultipart,
    ) -> Result<()> {
        let s3 = self
            .s3_signer
            .as_ref()
            .context("multipart upload reported to a store without S3")?;
        crate::multipart::complete(s3, &STATS, &self.object_path(hash), receipt).await
    }

    pub async fn abort_multipart(&self, hash: &str, upload_id: &str) {
        if let Some(s3) = &self.s3_signer {
            crate::multipart::abort(s3, &self.object_path(hash), upload_id).await;
        }
    }

    fn eval_cache_path(&self, key: &str) -> Path {
        Path::from(format!("{}{}", self.prefix, key))
    }

    pub async fn put_eval_cache(&self, key: &str, data: Vec<u8>) -> Result<()> {
        self.put_object(self.eval_cache_path(key), data).await
    }

    pub async fn get_eval_cache(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.get_object(&self.eval_cache_path(key)).await
    }

    pub async fn get_eval_cache_stream(
        &self,
        key: &str,
    ) -> Result<Option<(u64, BoxStream<'static, Result<Bytes>>)>> {
        self.get_stream_object(&self.eval_cache_path(key), None)
            .await
    }

    pub async fn presigned_eval_cache_get_url(
        &self,
        key: &str,
        expires_in: std::time::Duration,
    ) -> Result<Option<String>> {
        self.presign_object(reqwest::Method::GET, &self.eval_cache_path(key), expires_in)
            .await
    }

    pub async fn presigned_eval_cache_put_url(
        &self,
        key: &str,
        expires_in: std::time::Duration,
    ) -> Result<Option<String>> {
        self.presign_object(reqwest::Method::PUT, &self.eval_cache_path(key), expires_in)
            .await
    }

    pub async fn copy_eval_cache(&self, from_key: &str, to_key: &str) -> Result<()> {
        self.inner
            .copy(
                &self.eval_cache_path(from_key),
                &self.eval_cache_path(to_key),
            )
            .await
            .with_context(|| format!("copy eval-cache object {from_key} to {to_key}"))
    }

    pub async fn delete_eval_cache(&self, key: &str) -> Result<()> {
        self.delete_object(&self.eval_cache_path(key)).await
    }

    fn blob_path(&self, project: uuid::Uuid, hash: &[u8; 32]) -> Path {
        let hex = hex::encode(hash);
        let shard = &hex[..2];
        Path::from(format!(
            "{}build-request-blobs/{}/{}/{}",
            self.prefix, project, shard, hex,
        ))
    }

    pub async fn put_blob(
        &self,
        project: uuid::Uuid,
        hash: &[u8; 32],
        data: Vec<u8>,
    ) -> Result<()> {
        self.put_object(self.blob_path(project, hash), data).await
    }

    pub async fn get_blob(&self, project: uuid::Uuid, hash: &[u8; 32]) -> Result<Option<Vec<u8>>> {
        self.get_object(&self.blob_path(project, hash)).await
    }

    pub async fn delete_blob(&self, project: uuid::Uuid, hash: &[u8; 32]) -> Result<()> {
        self.delete_object(&self.blob_path(project, hash)).await
    }

    fn exec_output_path(&self, job: uuid::Uuid, path: &str) -> Path {
        Path::from(format!("{}{}", self.prefix, exec_output_key(job, path)))
    }

    pub async fn adopt_exec_output(
        &self,
        job: uuid::Uuid,
        path: &str,
        staged: &std::path::Path,
    ) -> Result<()> {
        self.adopt_into(self.exec_output_path(job, path), staged)
            .await
    }

    pub async fn open_exec_output(
        &self,
        job: uuid::Uuid,
        path: &str,
        offset: u64,
    ) -> Result<Option<(u64, BoxStream<'static, Result<Bytes>>)>> {
        self.get_stream_object(&self.exec_output_path(job, path), Some(offset))
            .await
    }

    pub async fn get_exec_output(
        &self,
        job: uuid::Uuid,
        path: &str,
        max: usize,
    ) -> Result<Option<Vec<u8>>> {
        let object = self.exec_output_path(job, path);
        match self.head_object(&object).await? {
            None => Ok(None),
            Some(meta) if meta.size > max as u64 => {
                anyhow::bail!(
                    "exec output {path} is {} bytes, the limit is {max}",
                    meta.size
                )
            }
            Some(_) => self.get_object(&object).await,
        }
    }

    pub async fn delete_exec_outputs(&self, job: uuid::Uuid) -> Result<()> {
        let listed = Path::from(format!("{}{EXEC_OUTPUTS_DIR}/{job}", self.prefix));
        let mut stream = self.inner.list(Some(&listed));
        while let Some(item) = stream.next().await {
            let meta = item.context("list exec outputs")?;
            self.delete_object(&meta.location).await?;
        }
        Ok(())
    }

    pub async fn list_exec_output_jobs(&self) -> Result<Vec<uuid::Uuid>> {
        let listed = Path::from(format!("{}{EXEC_OUTPUTS_DIR}", self.prefix));
        let mut stream = self.inner.list(Some(&listed));
        let mut jobs = std::collections::BTreeSet::new();
        while let Some(item) = stream.next().await {
            let location = item.context("list exec outputs")?.location.to_string();
            let job = location
                .strip_prefix(self.prefix.as_str())
                .and_then(|key| key.strip_prefix(EXEC_OUTPUTS_DIR))
                .and_then(|rest| rest.split('/').nth(1))
                .and_then(|job| uuid::Uuid::parse_str(job).ok());
            jobs.extend(job);
        }
        Ok(jobs.into_iter().collect())
    }

    pub fn shards() -> Vec<String> {
        let chars = gradient_util::nix_hash::NIX32_CHARS;
        chars
            .iter()
            .flat_map(|&a| {
                chars
                    .iter()
                    .map(move |&b| format!("{}{}", a as char, b as char))
            })
            .collect()
    }

    /// The orphan sweep is using the last-modified time to spare fresh NARs. An upload is landing
    /// before the eval is committing its rows, and reclaiming it would strand a zombie
    /// `cached_path`.
    pub async fn list_shard(&self, shard: &str) -> Result<Vec<(String, i64)>> {
        let prefix = Path::from(format!("{}nars/{shard}", self.prefix));
        let mut stream = self.inner.list(Some(&prefix));
        let mut hashes = Vec::new();
        while let Some(item) = stream.next().await {
            let meta = item.context("list NAR store")?;
            if let Some(stem) = meta
                .location
                .filename()
                .and_then(|name| name.strip_suffix(".nar.zst"))
            {
                hashes.push((format!("{shard}{stem}"), meta.last_modified.timestamp()));
            }
        }
        Ok(hashes)
    }

    pub async fn list_eval_cache_keys(&self) -> Result<Vec<String>> {
        let listed = Path::from(format!("{}eval-cache", self.prefix));
        let mut stream = self.inner.list(Some(&listed));
        let mut keys = Vec::new();
        while let Some(item) = stream.next().await {
            let location = item.context("list eval-cache")?.location.to_string();
            if let Some(key) = location.strip_prefix(self.prefix.as_str()) {
                keys.push(key.to_owned());
            }
        }
        Ok(keys)
    }

    pub async fn list_eval_cache_segments(
        &self,
        cache: CacheId,
        fingerprint: &str,
    ) -> Result<Vec<(String, u64)>> {
        let listed = self.eval_cache_path(&eval_cache_segment_prefix(cache, fingerprint));
        let mut stream = self.inner.list(Some(&listed));
        let mut segments = Vec::new();
        while let Some(item) = stream.next().await {
            let meta = item.context("list eval cache segments")?;
            let location = meta.location.to_string();
            if let Some(key) = location.strip_prefix(self.prefix.as_str()) {
                segments.push((key.to_owned(), meta.size));
            }
        }

        segments.sort_unstable();
        Ok(segments)
    }

    pub async fn delete_eval_cache_segments(
        &self,
        cache: CacheId,
        fingerprint: &str,
    ) -> Result<()> {
        for (key, _) in self.list_eval_cache_segments(cache, fingerprint).await? {
            self.delete_eval_cache(&key).await?;
        }

        Ok(())
    }

    pub async fn list_blobs(&self) -> Result<Vec<(uuid::Uuid, [u8; 32])>> {
        let prefix = Path::from(format!("{}build-request-blobs", self.prefix));
        let mut stream = self.inner.list(Some(&prefix));
        let mut out = Vec::new();
        while let Some(item) = stream.next().await {
            let meta = item.context("list build-request-blobs")?;
            let p = meta.location.to_string();
            let parts: Vec<&str> = p.split('/').collect();
            if parts.len() < 3 {
                continue;
            }
            let hash_hex = parts[parts.len() - 1];
            let project_part = parts[parts.len() - 3];
            let Ok(project) = uuid::Uuid::parse_str(project_part) else {
                tracing::debug!(path = %p, "skipping blob with non-uuid project dir");
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
            out.push((project, hash));
        }
        Ok(out)
    }
}

const EXEC_OUTPUTS_DIR: &str = "exec-outputs";

pub fn exec_output_key(job: uuid::Uuid, path: &str) -> String {
    format!("{EXEC_OUTPUTS_DIR}/{job}/{path}")
}

async fn local_stream(
    path: PathBuf,
    offset: u64,
) -> Result<Option<(u64, BoxStream<'static, Result<Bytes>>)>> {
    let mut file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).context("open local NAR"),
    };

    let size = file.metadata().await.context("stat local NAR")?.len();
    if offset >= size {
        return Ok(Some((size, futures::stream::empty().boxed())));
    }

    file.seek(SeekFrom::Start(offset))
        .await
        .context("seek local NAR")?;
    let stream = tokio_util::io::ReaderStream::with_capacity(file, BULK_CHUNK_SIZE)
        .map(|c| c.context("read local NAR"))
        .boxed();
    Ok(Some((size, stream)))
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

pub enum NarSource {
    Hot(Bytes),
    Stream {
        size: u64,
        stream: BoxStream<'static, Result<Bytes>>,
    },
}

impl NarSource {
    pub fn size(&self) -> u64 {
        match self {
            NarSource::Hot(bytes) => bytes.len() as u64,
            NarSource::Stream { size, .. } => *size,
        }
    }
}

async fn collect(mut stream: BoxStream<'static, Result<Bytes>>, size: u64) -> Result<Bytes> {
    let mut buf = BytesMut::with_capacity(usize::try_from(size).unwrap_or(0));
    while let Some(chunk) = stream.next().await {
        buf.extend_from_slice(&chunk?);
    }

    Ok(buf.freeze())
}

impl NarStore {
    pub async fn open(&self, hash: &str, offset: u64) -> Result<Option<NarSource>> {
        if let Some(bytes) = self.hot.get(hash) {
            return Ok(Some(NarSource::Hot(bytes)));
        }

        let Some((size, stream)) = self.get_stream_from(hash, offset).await? else {
            return Ok(None);
        };

        if offset == 0 && self.hot.admits(size) {
            let bytes = self.hot.get_or_load(hash, collect(stream, size)).await?;
            return Ok(Some(NarSource::Hot(bytes)));
        }

        Ok(Some(NarSource::Stream { size, stream }))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StorageTarget {
    Passthrough,
    Put { url: String },
    Multipart(gradient_wire::types::PresignedMultipart),
}

pub fn upload_lease(target: &StorageTarget, size: u64) -> Option<std::time::Duration> {
    match target {
        StorageTarget::Passthrough => None,
        StorageTarget::Put { .. } => Some(PRESIGN_TTL),
        StorageTarget::Multipart(_) => Some(crate::multipart::part_ttl(size)),
    }
}

impl NarStore {
    pub async fn upload_target(&self, object: &ObjectKey, size: u64) -> Result<StorageTarget> {
        if !self.presigner_available() {
            return Ok(StorageTarget::Passthrough);
        }
        let missing = || anyhow::anyhow!("the S3 store could not presign an upload for {object:?}");
        match object {
            ObjectKey::Rest(_) => Ok(StorageTarget::Passthrough),
            ObjectKey::Nar(hash) if size > MULTIPART_NAR_BYTES => self
                .presigned_multipart(hash, size)
                .await?
                .map(StorageTarget::Multipart)
                .ok_or_else(missing),
            ObjectKey::Nar(hash) => self
                .presigned_put_url(hash, PRESIGN_TTL)
                .await?
                .map(|url| StorageTarget::Put { url })
                .ok_or_else(missing),
            ObjectKey::EvalCache(key) => self
                .presigned_eval_cache_put_url(key, PRESIGN_TTL)
                .await?
                .map(|url| StorageTarget::Put { url })
                .ok_or_else(missing),
        }
    }

    pub async fn verify_eval_cache(&self, key: &str, size: u64) -> Result<()> {
        let meta = self
            .head_object(&self.eval_cache_path(key))
            .await?
            .with_context(|| format!("eval-cache object {key} is missing"))?;
        anyhow::ensure!(
            meta.size == size,
            "eval-cache object {key} is {} bytes, expected {size}",
            meta.size
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval_cache_segment::eval_cache_segment_key;
    use futures::TryStreamExt as _;
    use tempfile::TempDir;

    #[tokio::test]
    async fn a_shard_lists_only_its_own_nars_by_full_hash() {
        let dir = TempDir::new().unwrap();
        let store = NarStore::local(dir.path()).unwrap();
        let mine = format!("0a{}", "b".repeat(30));
        let other = format!("0b{}", "a".repeat(30));
        store.put(&mine, b"x".to_vec()).await.unwrap();
        store.put(&other, b"y".to_vec()).await.unwrap();

        let listed: Vec<String> = store
            .list_shard("0a")
            .await
            .unwrap()
            .into_iter()
            .map(|(hash, _)| hash)
            .collect();
        assert_eq!(listed, vec![mine]);
        assert!(store.list_shard("zz").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_local_store_passes_through_every_upload() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = NarStore::local(dir.path()).unwrap();
        for object in [
            ObjectKey::Nar("a".repeat(32)),
            ObjectKey::EvalCache("eval-cache/c/fp".into()),
        ] {
            assert_eq!(
                store
                    .upload_target(&object, 10 * 1024 * 1024 * 1024)
                    .await
                    .unwrap(),
                StorageTarget::Passthrough
            );
        }
    }

    #[tokio::test]
    async fn an_s3_store_presigns_nars_and_eval_cache_blobs() {
        let store = NarStore::s3(
            "bucket",
            "us-east-1",
            Some("http://127.0.0.1:1"),
            Some("key"),
            Some("secret"),
            "",
            false,
            S3Timeouts::default(),
        )
        .unwrap();
        let nar = store
            .upload_target(&ObjectKey::Nar("a".repeat(32)), 1024)
            .await
            .unwrap();
        assert!(matches!(nar, StorageTarget::Put { .. }), "{nar:?}");
        let blob = store
            .upload_target(&ObjectKey::EvalCache("eval-cache/c/fp".into()), 1024)
            .await
            .unwrap();
        assert!(matches!(blob, StorageTarget::Put { .. }), "{blob:?}");
        assert_eq!(
            store
                .upload_target(&ObjectKey::Rest("r".into()), 1)
                .await
                .unwrap(),
            StorageTarget::Passthrough
        );
    }

    fn local_store() -> (TempDir, NarStore) {
        let dir = TempDir::new().expect("tempdir");
        let store = NarStore::local(dir.path()).expect("local store");
        (dir, store)
    }

    fn tiered_store(threshold: u64) -> (TempDir, NarStore) {
        let dir = TempDir::new().expect("tempdir");
        let store = NarStore::local(dir.path())
            .expect("local store")
            .with_hot_cache(HotNarCache::new(4 * 1024 * 1024, threshold));
        (dir, store)
    }

    async fn collect_source(source: NarSource) -> Vec<u8> {
        match source {
            NarSource::Hot(bytes) => bytes.to_vec(),
            NarSource::Stream { mut stream, .. } => {
                let mut out = Vec::new();
                while let Some(chunk) = stream.next().await {
                    out.extend_from_slice(&chunk.expect("chunk"));
                }

                out
            }
        }
    }

    #[tokio::test]
    async fn open_answers_a_small_object_from_ram_on_the_second_call() {
        let (_d, store) = tiered_store(64 * 1024);
        let payload = vec![7u8; 1000];
        store.put("small", payload.clone()).await.expect("put");

        let first = store
            .open("small", 0)
            .await
            .expect("open")
            .expect("present");
        assert!(matches!(first, NarSource::Hot(_)), "read through into RAM");
        assert_eq!(collect_source(first).await, payload);
        let stats = store.hot().stats();
        assert_eq!((stats.entries, stats.hits, stats.misses), (1, 0, 1));

        let second = store
            .open("small", 0)
            .await
            .expect("open")
            .expect("present");
        assert!(matches!(second, NarSource::Hot(_)));
        assert_eq!(store.hot().stats().hits, 1);
    }

    #[tokio::test]
    async fn open_streams_a_large_object_and_never_caches_it() {
        let (_d, store) = tiered_store(64 * 1024);
        let payload: Vec<u8> = (0..=u8::MAX).cycle().take(128 * 1024).collect();
        store.put("large", payload.clone()).await.expect("put");

        let source = store
            .open("large", 0)
            .await
            .expect("open")
            .expect("present");
        assert!(matches!(source, NarSource::Stream { size, .. } if size == payload.len() as u64));
        assert_eq!(collect_source(source).await, payload);
        assert_eq!(store.hot().stats().entries, 0);
    }

    #[tokio::test]
    async fn open_at_an_offset_streams_the_tail_of_a_large_object() {
        let (_d, store) = tiered_store(64 * 1024);
        let payload: Vec<u8> = (0..=u8::MAX).cycle().take(128 * 1024).collect();
        store.put("large", payload.clone()).await.expect("put");

        let source = store
            .open("large", 100)
            .await
            .expect("open")
            .expect("present");
        assert_eq!(source.size(), payload.len() as u64);
        assert_eq!(collect_source(source).await, payload[100..]);
    }

    #[tokio::test]
    async fn open_is_none_for_an_unknown_hash() {
        let (_d, store) = tiered_store(64 * 1024);
        assert!(store.open("nothing", 0).await.expect("open").is_none());
    }

    #[tokio::test]
    async fn get_stream_returns_full_payload_in_order() {
        let (_d, store) = local_store();
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
        assert_eq!(usize::try_from(size).expect("size fits"), payload.len());

        let mut assembled = Vec::with_capacity(payload.len());
        while let Some(chunk) = stream.next().await {
            assembled.extend_from_slice(&chunk.expect("chunk"));
        }
        assert_eq!(assembled, payload);
    }

    #[tokio::test]
    async fn put_reader_round_trips_multipart_payload() {
        let (_d, store) = local_store();
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
    async fn a_copied_eval_cache_blob_reads_back_under_its_new_key() {
        let (_d, store) = local_store();
        store
            .put_eval_cache("eval-cache/a/fp", b"blob".to_vec())
            .await
            .expect("put");
        store
            .copy_eval_cache("eval-cache/a/fp", "eval-cache/b/fp")
            .await
            .expect("copy");
        assert_eq!(
            store.get_eval_cache("eval-cache/b/fp").await.expect("get"),
            Some(b"blob".to_vec())
        );
    }

    async fn put_segment(
        store: &NarStore,
        cache: CacheId,
        fingerprint: &str,
        len: usize,
    ) -> String {
        let key = eval_cache_segment_key(cache, fingerprint, uuid::Uuid::now_v7());
        store
            .put_eval_cache(&key, vec![0; len])
            .await
            .expect("put segment");
        key
    }

    #[tokio::test]
    async fn segments_list_in_key_order_with_sizes() {
        let (_d, store) = local_store();
        let cache = CacheId::now_v7();
        let keys = [
            eval_cache_segment_key(cache, "fp", uuid::Uuid::now_v7()),
            eval_cache_segment_key(cache, "fp", uuid::Uuid::now_v7()),
            eval_cache_segment_key(cache, "fp", uuid::Uuid::now_v7()),
        ];
        for (index, len) in [(2, 30), (0, 10), (1, 20)] {
            store
                .put_eval_cache(&keys[index], vec![0; len])
                .await
                .expect("put segment");
        }

        assert_eq!(
            store
                .list_eval_cache_segments(cache, "fp")
                .await
                .expect("list"),
            vec![
                (keys[0].clone(), 10),
                (keys[1].clone(), 20),
                (keys[2].clone(), 30),
            ]
        );
    }

    #[tokio::test]
    async fn segments_of_other_fingerprints_and_caches_are_not_listed() {
        let (_d, store) = local_store();
        let cache = CacheId::now_v7();
        let mine = put_segment(&store, cache, "fp", 4).await;
        put_segment(&store, cache, "fp2", 4).await;
        put_segment(&store, CacheId::now_v7(), "fp", 4).await;

        assert_eq!(
            store
                .list_eval_cache_segments(cache, "fp")
                .await
                .expect("list"),
            vec![(mine, 4)]
        );
    }

    #[tokio::test]
    async fn deleting_segments_removes_only_that_fingerprint() {
        let (_d, store) = local_store();
        let cache = CacheId::now_v7();
        put_segment(&store, cache, "fp", 4).await;
        put_segment(&store, cache, "fp", 4).await;
        let kept = put_segment(&store, cache, "fp2", 4).await;

        store
            .delete_eval_cache_segments(cache, "fp")
            .await
            .expect("delete");

        assert!(
            store
                .list_eval_cache_segments(cache, "fp")
                .await
                .expect("list")
                .is_empty()
        );
        assert_eq!(
            store
                .list_eval_cache_segments(cache, "fp2")
                .await
                .expect("list"),
            vec![(kept, 4)]
        );
    }

    #[tokio::test]
    async fn exists_reflects_presence() {
        let (_d, store) = local_store();
        assert!(!store.exists("ab12cd").await.expect("head"));
        store.put("ab12cd", b"data".to_vec()).await.expect("put");
        assert!(store.exists("ab12cd").await.expect("head"));
    }

    #[tokio::test]
    async fn a_local_read_records_get_latency() {
        use gradient_util::telemetry::metric;

        let gets = || -> i64 {
            STATS
                .snapshot()
                .iter()
                .filter(|(k, _)| k.metric == metric::STORAGE_OP_MS && k.label == "get")
                .map(|(_, a)| a.count)
                .sum()
        };

        let (_d, store) = local_store();
        store.put("ab12cd", b"data".to_vec()).await.expect("put");
        let before = gets();
        let _found = store
            .get_stream("ab12cd")
            .await
            .expect("read")
            .expect("found");

        assert!(gets() > before);
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
        assert_eq!(
            usize::try_from(size).expect("size fits"),
            payload.len(),
            "size is the FULL object size"
        );

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

    async fn adopt_output(dir: &TempDir, store: &NarStore, job: uuid::Uuid, path: &str) {
        let staged = dir.path().join("staged.partial");
        tokio::fs::write(&staged, b"abcdef").await.unwrap();
        store.adopt_exec_output(job, path, &staged).await.unwrap();
    }

    #[tokio::test]
    async fn an_adopted_output_reads_back_from_an_offset() {
        let (dir, store) = local_store();
        let job = uuid::Uuid::now_v7();
        adopt_output(&dir, &store, job, "model/weights.bin").await;
        let (size, stream) = store
            .open_exec_output(job, "model/weights.bin", 2)
            .await
            .unwrap()
            .expect("the output exists");
        assert_eq!(size, 6);
        assert_eq!(&collect(stream, size - 2).await.unwrap()[..], b"cdef");
        assert!(
            store
                .open_exec_output(job, "missing", 0)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn deleting_a_jobs_outputs_leaves_other_jobs() {
        let (dir, store) = local_store();
        let (gone, kept) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
        adopt_output(&dir, &store, gone, "a.txt").await;
        adopt_output(&dir, &store, gone, "nested/b.txt").await;
        adopt_output(&dir, &store, kept, "a.txt").await;
        store.delete_exec_outputs(gone).await.unwrap();
        assert_eq!(store.list_exec_output_jobs().await.unwrap(), vec![kept]);
        assert!(
            store
                .get_exec_output(gone, "a.txt", 64)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn an_output_above_the_read_cap_is_an_error() {
        let (dir, store) = local_store();
        let job = uuid::Uuid::now_v7();
        adopt_output(&dir, &store, job, "gradient/continue.json").await;
        assert!(
            store
                .get_exec_output(job, "gradient/continue.json", 5)
                .await
                .is_err()
        );
        assert_eq!(
            store
                .get_exec_output(job, "gradient/continue.json", 6)
                .await
                .unwrap()
                .unwrap(),
            b"abcdef"
        );
    }

    #[tokio::test]
    async fn adopt_file_renames_into_a_local_store() {
        let (dir, store) = local_store();
        let staged = dir.path().join("staged.partial");
        tokio::fs::write(&staged, b"zstd bytes").await.unwrap();
        store.adopt_file(&"a".repeat(32), &staged).await.unwrap();
        assert!(!staged.exists());
        assert_eq!(
            store.get(&"a".repeat(32)).await.unwrap().unwrap(),
            b"zstd bytes"
        );
    }

    #[tokio::test]
    async fn local_stream_from_reads_in_bulk_chunks_and_honours_offset() {
        let (_d, store) = local_store();
        let payload: Vec<u8> = (0..=u8::MAX)
            .cycle()
            .take(BULK_CHUNK_SIZE * 2 + 17)
            .collect();
        store.put(&"b".repeat(32), payload.clone()).await.unwrap();
        let (size, stream) = store
            .get_stream_from(&"b".repeat(32), 5)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(usize::try_from(size).expect("size fits"), payload.len());
        let chunks: Vec<Bytes> = stream.try_collect().await.unwrap();
        assert!(chunks.iter().all(|c| c.len() <= BULK_CHUNK_SIZE));
        assert_eq!(
            chunks[0].len(),
            BULK_CHUNK_SIZE,
            "reads are chunk-sized, not 8 KiB"
        );
        assert_eq!(chunks.concat(), &payload[5..]);
    }

    #[tokio::test]
    async fn get_stream_from_past_end_is_empty() {
        let (_d, store) = local_store();
        store.put("jkl012", b"abc".to_vec()).await.unwrap();
        let (size, mut stream) = store.get_stream_from("jkl012", 99).await.unwrap().unwrap();
        assert_eq!(size, 3);
        assert!(stream.next().await.is_none());
    }

    #[test]
    fn single_write_budget_scales_with_the_payload() {
        assert_eq!(single_write_budget(0), WRITE_STALL_TIMEOUT);
        assert_eq!(single_write_budget(2 * 1024), WRITE_STALL_TIMEOUT);
        let big = single_write_budget(256 * 1024 * 1024);
        assert!(big > WRITE_STALL_TIMEOUT * 8, "big blob budget: {big:?}");
        assert!(single_write_budget(16 * 1024 * 1024) > single_write_budget(1024));
    }

    #[tokio::test]
    async fn bounded_reports_the_operation_that_ran_out_of_time() {
        let err = bounded(
            std::future::pending::<std::result::Result<(), std::io::Error>>(),
            std::time::Duration::from_millis(1),
            "wedged write",
        )
        .await
        .expect_err("a future that never resolves must not hang the caller");
        assert!(
            format!("{err:#}").contains("wedged write"),
            "error must name the operation: {err:#}"
        );
    }

    #[test]
    fn retry_budget_outlasts_every_attempt_it_should_cover() {
        let t = S3Timeouts::default();
        let attempts = u32::try_from(t.max_retries + 1).expect("retry count fits");
        assert!(
            t.retry_timeout > t.read_timeout * attempts,
            "retry_timeout {:?} must exceed {attempts} attempts of {:?}",
            t.retry_timeout,
            t.read_timeout,
        );
        assert!(t.retry_timeout < std::time::Duration::from_secs(300));
    }
}
