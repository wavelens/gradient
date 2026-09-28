/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! NAR upload: [`upload_nar`] asks the server for a grant through the
//! [`UploadClient`] and moves the bytes the way the grant says - relayed
//! [`ClientMessage::UploadChunk`] frames, one presigned PUT, or presigned
//! multipart parts.

use std::io::Write as _;

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use gradient_util::nix_hash::nix32_encode;
use gradient_wire::messages::{ClientMessage, NAR_ZSTD_LEVEL};
use gradient_wire::session::frame::BULK_CHUNK_SIZE;
use sha2::{Digest, Sha256};
use tracing::debug;

use gradient_util::store_path::nix_store_path;

use crate::connection::ProtoWriter;
use crate::nar_multipart::{PartSink, PartUploader};
use crate::upload::UploadClient;
use gradient_wire::types::{
    CompletedMultipart, GrantTarget, NarUploadMetadata, UploadMetadata, UploadObject,
};

/// `sha256:<nix32>` of `data` - the wire format for NAR and file hashes.
pub fn sha256_nix32(data: &[u8]) -> String {
    format!("sha256:{}", nix32_encode(&Sha256::digest(data)))
}

fn finalize_nix32(hasher: Sha256) -> String {
    format!("sha256:{}", nix32_encode(&hasher.finalize()))
}

/// Upper bound on zstd worker threads for one NAR. A few cores shorten a
/// multi-GB output noticeably; more would starve the build running alongside.
fn max_compression_threads() -> u32 {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(4) as u32
}

/// Below this a ZSTDMT context - a worker pool plus window-sized job buffers,
/// built and torn down per NAR - costs more than the parallelism buys, and an
/// eval pushes hundreds of such paths.
const MT_MIN_BYTES: u64 = 8 * 1024 * 1024;

/// Worker threads for a NAR of `size_hint` uncompressed bytes. `None` is a
/// source whose size is unknown before packing, where a large output is the
/// case worth optimising for.
fn compression_threads(size_hint: Option<u64>) -> u32 {
    match size_hint {
        Some(size) if size < MT_MIN_BYTES => 1,
        _ => max_compression_threads(),
    }
}

/// The one zstd encoder every NAR upload goes through, so level and thread
/// count cannot drift between the relay, presigned and substitute-relay paths.
/// One worker still builds a ZSTDMT context, so a `threads` of 1 stays on the
/// plain single-threaded encoder.
pub fn nar_encoder<W: std::io::Write>(
    sink: W,
    threads: u32,
) -> Result<zstd::stream::Encoder<'static, W>> {
    let mut encoder = zstd::stream::Encoder::new(sink, NAR_ZSTD_LEVEL)
        .context("failed to create zstd encoder")?;
    if threads > 1 {
        encoder
            .multithread(threads)
            .context("enable multithreaded zstd")?;
    }
    Ok(encoder)
}

/// Decide what slice of a freshly-produced compressed `part` to send when
/// resuming from `resume_from`. `produced` is the absolute offset of this
/// part's first byte. Returns `(absolute_offset, range_within_part)`, or `None`
/// when the part lies entirely before the resume point.
fn trim_for_resume(
    part_len: usize,
    produced: u64,
    resume_from: u64,
) -> Option<(u64, std::ops::Range<usize>)> {
    let end = produced + part_len as u64;
    if end <= resume_from {
        None
    } else if produced >= resume_from {
        Some((produced, 0..part_len))
    } else {
        let skip = (resume_from - produced) as usize;
        Some((resume_from, skip..part_len))
    }
}

/// [`trim_for_resume`] applied to an owned part: the untrimmed common case
/// hands the buffer straight to the frame instead of copying it again.
fn part_to_send(part: Vec<u8>, produced: u64, resume_from: u64) -> Option<(u64, Vec<u8>)> {
    let (offset, range) = trim_for_resume(part.len(), produced, resume_from)?;
    if range.start == 0 && range.end == part.len() {
        Some((offset, part))
    } else {
        Some((offset, part[range].to_vec()))
    }
}

// ── Source ────────────────────────────────────────────────────────────────────

/// Where the NAR bytes come from.
pub enum NarSource<'a> {
    /// Pack the store path from the local filesystem, compressing and hashing
    /// on the fly. References and deriver come from `meta` when provided
    /// (eval-internal pushes and tests pass `None`).
    Path {
        meta: Option<&'a dyn PathMetaSource>,
    },
    /// An uncompressed NAR already in memory, with the narinfo facts that travel
    /// with it. Compressed and hashed here, so nothing upstream is trusted for
    /// the metadata the server stores.
    Raw {
        nar: Vec<u8>,
        references: Vec<String>,
        deriver: Option<String>,
        ca: Option<String>,
    },
}

/// Compressed-NAR metadata reported in [`ClientMessage::UploadFinished`].
/// `file_*` describe the compressed object actually stored; `nar_*` describe
/// the uncompressed NAR. All hashes are `sha256:<nix32>`.
#[derive(Clone)]
pub struct CompressedNarMeta {
    pub file_hash: String,
    pub file_size: u64,
    pub nar_hash: String,
    pub nar_size: u64,
}

// ── The one uploader ──────────────────────────────────────────────────────────

/// Upload one NAR from `source` once the server grants it, and wait for its
/// acknowledgement.
pub async fn upload_nar(
    uploads: &UploadClient,
    job_id: &str,
    store_path: &str,
    source: NarSource<'_>,
) -> Result<()> {
    let store_path = nix_store_path(store_path);
    let object = UploadObject::Nar {
        store_path: store_path.clone(),
    };
    match source {
        NarSource::Path { meta } => {
            let path_meta = resolve_path_meta(meta, &store_path).await?;
            let nar_size = match path_meta.nar_size {
                Some(size) => size,
                None => measure_nar(&store_path).await?,
            };
            let threads = compression_threads(Some(nar_size));
            let mut upload = uploads.start(job_id, object, nar_size).await?;
            while let Some((request_id, target)) = upload.next_grant().await? {
                let sent = send_path(uploads.writer(), request_id, &store_path, threads, target)
                    .await
                    .map(|(meta, multipart)| nar_metadata(meta, &path_meta, multipart));
                if upload.settle(sent).await? {
                    break;
                }
            }
            Ok(())
        }
        NarSource::Raw {
            nar,
            references,
            deriver,
            ca,
        } => {
            let nar_size = nar.len() as u64;
            let nar = std::sync::Arc::new(nar);
            let path_meta = PathMeta {
                nar_size: Some(nar_size),
                references,
                deriver,
                ca,
            };
            let mut upload = uploads.start(job_id, object, nar_size).await?;
            while let Some((request_id, target)) = upload.next_grant().await? {
                let sent = send_raw(uploads.writer(), request_id, &nar, target)
                    .await
                    .map(|(meta, multipart)| nar_metadata(meta, &path_meta, multipart));
                if upload.settle(sent).await? {
                    break;
                }
            }
            Ok(())
        }
    }
}

async fn send_raw(
    writer: &ProtoWriter,
    request_id: u64,
    nar: &std::sync::Arc<Vec<u8>>,
    target: GrantTarget,
) -> Result<(CompressedNarMeta, Option<CompletedMultipart>)> {
    let nar = std::sync::Arc::clone(nar);
    let (compressed, meta) = tokio::task::spawn_blocking(move || compress_nar(&nar))
        .await
        .context("compress task panicked")??;
    let multipart = send_compressed(writer, request_id, compressed, target).await?;
    Ok((meta, multipart))
}

async fn send_path(
    writer: &ProtoWriter,
    request_id: u64,
    store_path: &str,
    threads: u32,
    target: GrantTarget,
) -> Result<(CompressedNarMeta, Option<CompletedMultipart>)> {
    match target {
        GrantTarget::Relay { resume_offset } => {
            debug!(store_path, resume_offset, "relayed NAR upload");
            let started = std::time::Instant::now();
            let mut relay = RelayStream::new(request_id, writer, resume_offset);
            let meta = pack_path_in_parts(store_path, threads, BULK_CHUNK_SIZE, &mut relay).await?;
            let sent = relay.finish().await?;
            crate::throughput::NETWORK.observe_transfer(sent, started.elapsed());
            Ok((meta, None))
        }
        GrantTarget::Put { url } => {
            debug!(store_path, "presigned NAR upload");
            let (compressed, meta) = pack_compress_path(store_path, threads).await?;
            http_put(&url, compressed).await?;
            Ok((meta, None))
        }
        GrantTarget::Multipart(grant) => {
            debug!(
                store_path,
                parts = grant.part_urls.len(),
                "presigned multipart NAR upload"
            );
            let mut uploader = PartUploader::new(&grant);
            let meta = pack_path_in_parts(store_path, threads, uploader.part_size(), &mut uploader)
                .await?;
            Ok((meta, Some(uploader.finish().await?)))
        }
        GrantTarget::Skip => bail!("a skipped upload has no transfer"),
    }
}

async fn send_compressed(
    writer: &ProtoWriter,
    request_id: u64,
    compressed: Vec<u8>,
    target: GrantTarget,
) -> Result<Option<CompletedMultipart>> {
    match target {
        GrantTarget::Relay { resume_offset } => {
            let mut relay = RelayStream::new(request_id, writer, resume_offset);
            for part in compressed.chunks(BULK_CHUNK_SIZE) {
                relay.send_part(part.to_vec()).await?;
            }
            relay.finish().await?;
            Ok(None)
        }
        GrantTarget::Put { url } => {
            http_put(&url, compressed).await?;
            Ok(None)
        }
        GrantTarget::Multipart(grant) => {
            let mut uploader = PartUploader::new(&grant);
            for part in compressed.chunks(uploader.part_size()) {
                uploader.send_part(part.to_vec()).await?;
            }
            Ok(Some(uploader.finish().await?))
        }
        GrantTarget::Skip => bail!("a skipped upload has no transfer"),
    }
}

fn nar_metadata(
    meta: CompressedNarMeta,
    path: &PathMeta,
    multipart: Option<CompletedMultipart>,
) -> UploadMetadata {
    UploadMetadata::Nar(Box::new(NarUploadMetadata {
        file_hash: meta.file_hash,
        file_size: meta.file_size,
        nar_size: meta.nar_size,
        nar_hash: meta.nar_hash,
        references: path.references.clone(),
        deriver: path.deriver.clone(),
        ca: path.ca.clone(),
        multipart,
    }))
}

/// Uncompressed NAR size of a path whose metadata source gave none: the upload
/// request must name its size before any byte is compressed.
async fn measure_nar(store_path: &str) -> Result<u64> {
    let mut nar_stream = harmonia_file_nar::NarByteStream::new(store_path.to_owned().into());
    let mut size = 0u64;
    while let Some(chunk) = nar_stream.next().await {
        size += chunk.context("NAR stream error")?.len() as u64;
    }
    Ok(size)
}

// ── Relay transport ───────────────────────────────────────────────────────────

/// One granted relay stream: parts trimmed against the server's resume offset,
/// closed with the empty final chunk.
struct RelayStream<'a> {
    request_id: u64,
    writer: &'a ProtoWriter,
    resume_from: u64,
    produced: u64,
}

impl<'a> RelayStream<'a> {
    fn new(request_id: u64, writer: &'a ProtoWriter, resume_from: u64) -> Self {
        Self {
            request_id,
            writer,
            resume_from,
            produced: 0,
        }
    }

    /// Sends the empty final chunk at the full size; returns the bytes this
    /// stream sent past the resume offset.
    async fn finish(self) -> Result<u64> {
        self.writer
            .send(ClientMessage::UploadChunk {
                request_id: self.request_id,
                data: Vec::new(),
                offset: self.produced,
                is_final: true,
            })
            .await?;
        Ok(self.produced.saturating_sub(self.resume_from))
    }
}

impl PartSink for RelayStream<'_> {
    async fn send_part(&mut self, part: Vec<u8>) -> Result<()> {
        let part_len = part.len() as u64;
        if let Some((offset, data)) = part_to_send(part, self.produced, self.resume_from) {
            self.writer
                .send(ClientMessage::UploadChunk {
                    request_id: self.request_id,
                    data,
                    offset,
                    is_final: false,
                })
                .await?;
        }
        self.produced += part_len;
        Ok(())
    }
}

/// Pack + compress `store_path` on `threads` zstd workers into `part_size`
/// pieces for `sink`, hashing as they go so nothing is buffered beyond one
/// part. The encoder's final flush is split the same way: a multithreaded
/// encoder holds whole jobs back until `finish`, so that tail runs to tens of
/// MiB on a large source, and one relay frame over `MAX_PROTO_MESSAGE_SIZE`
/// closes the session and fails the job.
async fn pack_path_in_parts(
    store_path: &str,
    threads: u32,
    part_size: usize,
    sink: &mut impl PartSink,
) -> Result<CompressedNarMeta> {
    let mut nar_stream = harmonia_file_nar::NarByteStream::new(store_path.to_owned().into());
    let mut encoder = nar_encoder(Vec::with_capacity(part_size * 2), threads)?;
    let mut file_hasher = Sha256::new();
    let mut nar_hasher = Sha256::new();
    let mut nar_size: u64 = 0;
    let mut file_size: u64 = 0;

    while let Some(chunk_result) = nar_stream.next().await {
        let chunk = chunk_result.context("NAR stream error")?;
        nar_hasher.update(&chunk);
        nar_size += chunk.len() as u64;
        encoder
            .write_all(&chunk)
            .context("zstd compression failed")?;

        let buf = encoder.get_mut();
        while buf.len() >= part_size {
            let part: Vec<u8> = buf.drain(..part_size).collect();
            file_hasher.update(&part);
            file_size += part.len() as u64;
            sink.send_part(part).await?;
        }
    }

    let remaining = encoder.finish().context("failed to finish zstd encoder")?;
    for part in remaining.chunks(part_size) {
        file_hasher.update(part);
        file_size += part.len() as u64;
        sink.send_part(part.to_vec()).await?;
    }

    Ok(CompressedNarMeta {
        file_hash: finalize_nix32(file_hasher),
        file_size,
        nar_hash: finalize_nix32(nar_hasher),
        nar_size,
    })
}

// ── Presigned transport ───────────────────────────────────────────────────────

/// Pack + compress `store_path` fully into memory (presigned PUT needs a
/// Content-Length, so the compressed body cannot stream).
async fn pack_compress_path(
    store_path: &str,
    threads: u32,
) -> Result<(Vec<u8>, CompressedNarMeta)> {
    let mut nar_stream = harmonia_file_nar::NarByteStream::new(store_path.to_owned().into());
    let mut encoder = nar_encoder(Vec::new(), threads)?;
    let mut nar_hasher = Sha256::new();
    let mut nar_size: u64 = 0;

    while let Some(chunk_result) = nar_stream.next().await {
        let chunk = chunk_result.context("NAR stream error")?;
        nar_hasher.update(&chunk);
        nar_size += chunk.len() as u64;
        encoder
            .write_all(&chunk)
            .context("zstd compression failed")?;
    }

    let compressed = encoder.finish().context("failed to finish zstd encoder")?;
    let meta = CompressedNarMeta {
        file_hash: sha256_nix32(&compressed),
        file_size: compressed.len() as u64,
        nar_hash: finalize_nix32(nar_hasher),
        nar_size,
    };
    Ok((compressed, meta))
}

async fn http_put(url: &str, body: Vec<u8>) -> Result<()> {
    crate::object_put::put_object(url, body.into(), Some("application/x-nix-nar")).await?;
    Ok(())
}

// ── Compression helper for the relay path ─────────────────────────────────────

/// Zstd-compress a raw in-memory NAR at [`NAR_ZSTD_LEVEL`] and return the
/// compressed bytes plus their [`CompressedNarMeta`]. Used by the substitute
/// relay to build a [`NarSource::Compressed`] when the upstream's encoding
/// cannot be stored verbatim.
pub fn compress_nar(raw_nar: &[u8]) -> Result<(Vec<u8>, CompressedNarMeta)> {
    let nar_size = raw_nar.len() as u64;
    let nar_hash = sha256_nix32(raw_nar);

    let mut encoder = nar_encoder(
        Vec::with_capacity(raw_nar.len() / 2),
        compression_threads(Some(nar_size)),
    )?;
    encoder
        .write_all(raw_nar)
        .context("zstd compression failed")?;
    let compressed = encoder.finish().context("failed to finish zstd encoder")?;

    let meta = CompressedNarMeta {
        file_hash: sha256_nix32(&compressed),
        file_size: compressed.len() as u64,
        nar_hash,
        nar_size,
    };
    Ok((compressed, meta))
}

// ── Local path metadata ───────────────────────────────────────────────────────

/// Path metadata a NAR push confirms with, gathered before the upload.
#[derive(Debug, Default, Clone)]
pub struct PathMeta {
    /// Uncompressed NAR size, used as the encoder's size hint. `None` when
    /// there is nothing to ask.
    pub nar_size: Option<u64>,
    /// Store-path references in hash-name format (no `/nix/store/` prefix).
    pub references: Vec<String>,
    /// Full deriver `.drv` path, if known.
    pub deriver: Option<String>,
    /// Content address in narinfo form, if known.
    pub ca: Option<String>,
}

/// Where a path's references, deriver and content address come from: the
/// worker's nix-daemon, or whatever store the proxy keeps.
#[async_trait::async_trait]
pub trait PathMetaSource: Send + Sync {
    /// `None` when the path is unknown or the source cannot be reached.
    async fn path_meta(&self, store_path: &str) -> Option<PathMeta>;
}

/// Resolve the metadata an upload should attach to a NAR push.
///
/// When the caller passes `None` (no local store - eval-internal pushes,
/// tests), the server is left to record whatever metadata it derives on its
/// side; we emit an empty [`PathMeta`].
///
/// When a source *is* provided, a missing answer is a hard error:
/// silently uploading with empty references stores a permanently incomplete
/// `cached_path` row, and a later build worker's prefetch closure walk then
/// misses references parsed straight out of the `.drv` content - the daemon
/// rejects the import with `path '…' is not valid`. Failing here keeps the
/// blame at the right layer and lets the caller retry instead of poisoning
/// the cache.
async fn resolve_path_meta(
    meta: Option<&dyn PathMetaSource>,
    store_path: &str,
) -> Result<PathMeta> {
    let Some(source) = meta else {
        return Ok(PathMeta::default());
    };
    source.path_meta(store_path).await.ok_or_else(|| {
        anyhow::anyhow!(
            "path metadata unavailable for {}; refusing to push NAR with incomplete cache entry",
            store_path
        )
    })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::disallowed_methods,
        reason = "tests stand in for their peers by hand"
    )]

    use super::*;
    use gradient_wire::testing::MockProtoServer;
    use gradient_wire::testing::ServedUpload;
    use std::time::Duration;

    struct Unreachable;

    #[async_trait::async_trait]
    impl PathMetaSource for Unreachable {
        async fn path_meta(&self, _: &str) -> Option<PathMeta> {
            None
        }
    }

    struct Known(PathMeta);

    #[async_trait::async_trait]
    impl PathMetaSource for Known {
        async fn path_meta(&self, _: &str) -> Option<PathMeta> {
            Some(self.0.clone())
        }
    }

    fn store_path(name: &str) -> String {
        format!("/nix/store/{}-{name}", "b".repeat(32))
    }

    async fn client(url: &str) -> (UploadClient, tokio::task::JoinHandle<()>) {
        let conn = crate::connection::ProtoConnection::open(url).await.unwrap();
        let (writer, mut reader, _flush) = conn.split();
        let uploads = UploadClient::new(writer, 4);
        let delivered = uploads.clone();
        let pump = tokio::spawn(async move {
            while let Some(inbound) = reader.recv().await {
                if let gradient_wire::Inbound::Control(msg) = inbound {
                    delivered.deliver(msg);
                }
            }
        });
        (uploads, pump)
    }

    /// Run `upload` against a mock that grants it `target` and acknowledges it.
    async fn served(
        target: GrantTarget,
        upload: impl AsyncFnOnce(&UploadClient) -> Result<()>,
    ) -> ServedUpload {
        let server = MockProtoServer::bind().await;
        let url = server.url().to_owned();
        let script = tokio::spawn(async move {
            let mut sc = server.accept().await;
            sc.serve_upload(target).await.unwrap()
        });
        let (uploads, pump) = client(&url).await;
        upload(&uploads).await.unwrap();
        pump.abort();
        script.await.unwrap()
    }

    /// Create a temporary directory with a single file and return its path.
    ///
    /// Each call produces a unique directory (UUID-based) so parallel tests
    /// don't interfere with each other's cleanup.
    fn make_temp_store_path() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gradient-nar-test-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("hello"), b"gradient nar test data").unwrap();
        dir
    }

    /// The references and deriver a push confirms with come from the path's
    /// metadata source, not from anything the NAR bytes could tell us.
    #[tokio::test]
    async fn path_relay_carries_the_sources_references() {
        let dir = make_temp_store_path();
        let path = dir.to_str().unwrap().to_owned();
        let deriver = store_path("x.drv");
        let meta = Known(PathMeta {
            references: vec![format!("{}-dep", "c".repeat(32))],
            deriver: Some(deriver.clone()),
            ..Default::default()
        });

        let served = served(GrantTarget::Relay { resume_offset: 0 }, async |uploads| {
            upload_nar(
                uploads,
                "job-123",
                &path,
                NarSource::Path { meta: Some(&meta) },
            )
            .await
        })
        .await;

        assert_eq!(served.job_id, "job-123");
        assert_eq!(served.nar().references, [format!("{}-dep", "c".repeat(32))]);
        assert_eq!(served.nar().deriver.as_deref(), Some(deriver.as_str()));
        assert_eq!(
            served.size,
            served.nar().nar_size,
            "the request names the NAR size"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn path_relay_sends_contiguous_zstd_chunks_and_an_empty_final() {
        let dir = make_temp_store_path();
        let path = dir.to_str().unwrap().to_owned();

        let served = served(GrantTarget::Relay { resume_offset: 0 }, async |uploads| {
            upload_nar(uploads, "job-123", &path, NarSource::Path { meta: None }).await
        })
        .await;

        let mut expected = 0u64;
        for (data, offset, _) in &served.chunks {
            assert_eq!(*offset, expected, "chunks are contiguous");
            expected += data.len() as u64;
        }
        let (last, _, is_final) = served.chunks.last().unwrap();
        assert!(
            *is_final && last.is_empty(),
            "the stream closes with an empty final chunk"
        );
        let relayed = served.relayed();
        assert_eq!(relayed.len() as u64, served.nar().file_size);
        assert_eq!(sha256_nix32(&relayed), served.nar().file_hash);
        let decoded = zstd::decode_all(relayed.as_slice()).unwrap();
        assert_eq!(decoded.len() as u64, served.nar().nar_size);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A multithreaded encoder emits nothing for a job until it completes and
    /// flushes every pending job at `finish`, so the tail of a large source
    /// arrives in one piece. It must still leave as bulk chunks: the server caps
    /// a frame at `MAX_PROTO_MESSAGE_SIZE` and closes the session on the first
    /// one over it, which failed every fetch-only build of a big tarball.
    #[tokio::test]
    async fn a_large_final_flush_is_split_into_bulk_chunks() {
        let dir = make_temp_store_path();
        let mut noise = vec![0u8; 12 * 1024 * 1024];
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        for byte in noise.iter_mut() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *byte = state as u8;
        }
        std::fs::write(dir.join("noise"), &noise).unwrap();
        let path = dir.to_str().unwrap().to_owned();

        let served = served(GrantTarget::Relay { resume_offset: 0 }, async |uploads| {
            upload_nar(uploads, "job-tail", &path, NarSource::Path { meta: None }).await
        })
        .await;

        for (data, offset, _) in &served.chunks {
            assert!(
                data.len() <= BULK_CHUNK_SIZE,
                "a {} byte frame at offset {offset} exceeds the bulk chunk",
                data.len()
            );
        }
        let file_size = served.nar().file_size;
        assert_eq!(
            served.chunks.last().unwrap().1,
            file_size,
            "the final offset is the bytes sent"
        );
        assert!(
            file_size > 8 * 1024 * 1024,
            "the noise must not compress away, or the tail never exceeds a chunk"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_resumed_relay_sends_only_the_tail() {
        let raw = b"a raw nar long enough to resume part of it".to_vec();
        let (compressed, _) = compress_nar(&raw).unwrap();

        let served = served(GrantTarget::Relay { resume_offset: 3 }, async |uploads| {
            let source = NarSource::Raw {
                nar: raw.clone(),
                references: vec![],
                deriver: None,
                ca: None,
            };
            upload_nar(uploads, "job-resume", &store_path("r"), source).await
        })
        .await;

        assert_eq!(served.chunks.first().unwrap().1, 3);
        assert_eq!(served.relayed(), compressed[3..]);
    }

    #[test]
    fn trim_for_resume_skips_trims_and_passes() {
        // Three 100-byte parts, resume from 150: part0 skipped, part1 trimmed
        // to its back half at offset 150, part2 sent whole at offset 200.
        assert_eq!(super::trim_for_resume(100, 0, 150), None);
        assert_eq!(super::trim_for_resume(100, 100, 150), Some((150, 50..100)));
        assert_eq!(super::trim_for_resume(100, 200, 150), Some((200, 0..100)));
        // resume_from 0 sends everything from offset 0.
        assert_eq!(super::trim_for_resume(100, 0, 0), Some((0, 0..100)));
    }

    /// A small NAR stays on the plain encoder: an eval pushes hundreds of them
    /// and a ZSTDMT context per path costs more than it saves.
    #[test]
    fn small_nars_compress_single_threaded() {
        assert_eq!(super::compression_threads(Some(1024)), 1);
        assert_eq!(
            super::compression_threads(Some(super::MT_MIN_BYTES)),
            super::max_compression_threads()
        );
        assert_eq!(
            super::compression_threads(None),
            super::max_compression_threads()
        );
    }

    /// The untrimmed part - every chunk of every fresh upload - must reach the
    /// frame as the very buffer the encoder produced, with no second copy.
    #[test]
    fn part_to_send_moves_an_untrimmed_part() {
        let part = vec![7u8; 100];
        let src = part.as_ptr();
        let (offset, sent) = super::part_to_send(part, 200, 150).expect("part is past the resume");
        assert_eq!(offset, 200);
        assert_eq!(sent.as_ptr(), src, "an untrimmed part must not be copied");

        let (offset, sent) =
            super::part_to_send(vec![7u8; 100], 100, 150).expect("part straddles the resume");
        assert_eq!((offset, sent.len()), (150, 50));

        assert!(super::part_to_send(vec![7u8; 100], 0, 150).is_none());
    }

    /// Minimal HTTP server that accepts one PUT and replies 200.
    async fn one_shot_http_server() -> (String, tokio::task::JoinHandle<Vec<u8>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{port}/upload");

        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 65536];
            let mut total = 0;
            loop {
                let n = stream.read(&mut buf[total..]).await.unwrap();
                if n == 0 {
                    break;
                }
                total += n;
                if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            buf[..total].to_vec()
        });

        (url, handle)
    }

    #[tokio::test]
    async fn path_presigned_puts_and_finishes_with_its_metadata() {
        let dir = make_temp_store_path();
        let path = dir.to_str().unwrap().to_owned();
        let (http_url, http_task) = one_shot_http_server().await;

        let served = served(GrantTarget::Put { url: http_url }, async |uploads| {
            upload_nar(uploads, "job-xyz", &path, NarSource::Path { meta: None }).await
        })
        .await;

        assert!(
            served.chunks.is_empty(),
            "a presigned upload relays nothing"
        );
        let meta = served.nar();
        assert!(meta.file_size > 0 && meta.nar_size > 0);
        assert!(meta.file_hash.starts_with("sha256:"), "{}", meta.file_hash);
        assert!(meta.nar_hash.starts_with("sha256:"), "{}", meta.nar_hash);
        assert!(
            !http_task.await.unwrap().is_empty(),
            "the PUT reached the URL"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn path_multipart_uploads_parts_and_reports_their_etags() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, Request, ResponseTemplate};

        let dir = make_temp_store_path();
        let path = dir.to_str().unwrap().to_owned();

        let s3 = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(|req: &Request| {
                ResponseTemplate::new(200).insert_header("ETag", format!("\"{}\"", req.url.path()))
            })
            .mount(&s3)
            .await;
        let grant = gradient_wire::types::PresignedMultipart {
            upload_id: "up-1".into(),
            part_size: 16,
            part_urls: (1..=4096).map(|i| format!("{}/{i:05}", s3.uri())).collect(),
        };

        let served = served(GrantTarget::Multipart(grant), async |uploads| {
            upload_nar(
                uploads,
                "job-multipart",
                &path,
                NarSource::Path { meta: None },
            )
            .await
        })
        .await;

        let meta = served.nar();
        let receipt = meta.multipart.as_ref().expect("a multipart receipt");
        let mut parts = s3.received_requests().await.unwrap();
        parts.sort_by(|a, b| a.url.path().cmp(b.url.path()));
        let expected_etags: Vec<String> = parts
            .iter()
            .map(|r| format!("\"{}\"", r.url.path()))
            .collect();
        assert_eq!(receipt.upload_id, "up-1");
        assert_eq!(receipt.etags, expected_etags);
        assert!(parts.len() > 1, "a 16-byte part size must split the NAR");
        let object: Vec<u8> = parts.iter().flat_map(|r| r.body.clone()).collect();
        assert_eq!(object.len() as u64, meta.file_size);
        assert_eq!(sha256_nix32(&object), meta.file_hash);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression: silently uploading with empty path metadata produced
    /// `cached_path` rows whose `references` column was `NULL`. A later
    /// build worker's prefetch then missed the `.drv`'s input_sources,
    /// the daemon parsed the `.drv` content, found the unstated reference,
    /// and aborted with `path '...' is not valid`. The upload must fail
    /// loudly, before it even asks for a grant.
    #[tokio::test]
    async fn an_upload_without_path_metadata_fails_before_its_request() {
        let dir = make_temp_store_path();
        let path = dir.to_str().unwrap().to_owned();
        let server = MockProtoServer::bind().await;
        let url = server.url().to_owned();
        let script = tokio::spawn(async move {
            let mut sc = server.accept().await;
            tokio::time::timeout(Duration::from_millis(300), sc.recv())
                .await
                .is_err()
        });
        let (uploads, _pump) = client(&url).await;

        let result = upload_nar(
            &uploads,
            "job-meta",
            &path,
            NarSource::Path {
                meta: Some(&Unreachable),
            },
        )
        .await;

        assert!(result.is_err());
        assert!(script.await.unwrap(), "no upload request was sent");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn raw_relay_compresses_streams_and_confirms() {
        let raw = b"verbatim upstream nar payload".to_vec();

        let served = served(GrantTarget::Relay { resume_offset: 0 }, async |uploads| {
            let source = NarSource::Raw {
                nar: raw.clone(),
                references: vec![],
                deriver: None,
                ca: None,
            };
            upload_nar(uploads, "job-raw", &store_path("raw"), source).await
        })
        .await;

        assert_eq!(zstd::decode_all(served.relayed().as_slice()).unwrap(), raw);
        assert_eq!(served.nar().nar_hash, sha256_nix32(&raw));
        assert_eq!(served.size, raw.len() as u64);
    }

    #[tokio::test]
    async fn raw_source_threads_content_address() {
        let ca = "fixed:r:sha256:1b8m03r63zqhnjf7l5wnldhh7c134ap5vpj0850ymkq1iyzicy5s";
        let (http_url, _http_task) = one_shot_http_server().await;

        let served = served(GrantTarget::Put { url: http_url }, async |uploads| {
            let source = NarSource::Raw {
                nar: b"abc".to_vec(),
                references: vec![],
                deriver: None,
                ca: Some(ca.to_owned()),
            };
            upload_nar(uploads, "job-ca", &store_path("hello-2.12"), source).await
        })
        .await;

        assert_eq!(served.nar().ca.as_deref(), Some(ca));
    }

    /// A raw NAR compressed here and PUT to a presigned URL, confirmed with the
    /// metadata this side computed.
    #[tokio::test]
    async fn raw_presigned_puts_and_confirms() {
        let raw = b"verbatim upstream nar payload".to_vec();
        let (http_url, http_task) = one_shot_http_server().await;

        let served = served(GrantTarget::Put { url: http_url }, async |uploads| {
            let source = NarSource::Raw {
                nar: raw.clone(),
                references: vec![],
                deriver: None,
                ca: None,
            };
            upload_nar(uploads, "job-verbatim", &store_path("verbatim"), source).await
        })
        .await;

        assert_eq!(served.nar().nar_hash, sha256_nix32(&raw));
        assert_eq!(served.nar().nar_size, raw.len() as u64);
        assert!(
            !http_task.await.unwrap().is_empty(),
            "the PUT reached the URL"
        );
    }
}
