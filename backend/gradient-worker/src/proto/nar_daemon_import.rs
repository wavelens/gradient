/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::io::Read as _;
use std::sync::Arc;

use anyhow::{Context, Result};
use gradient_wire::messages::CachedPath;
use harmonia_protocol::valid_path_info::ValidPathInfo;
use harmonia_store_path::StorePath;
use sha2::{Digest as _, Sha256};
use tracing::{debug, warn};

use crate::nix::store::LocalNixStore;
use crate::proto::compression::build_unkeyed_path_info;
use crate::proto::prefetch::CorruptCachedNar;
use gradient_worker_client::compression::{
    SNIFF_BYTES, decoder, parse_nar_hash_to_bytes, resolve_compression,
};
use gradient_worker_client::nar::NarReader;
use gradient_worker_client::nar_recv::NarPayload;

struct NarImporter<'a> {
    store: &'a LocalNixStore,
    store_path: &'a str,
    meta: &'a CachedPath,
}

impl<'a> NarImporter<'a> {
    fn new(store: &'a LocalNixStore, store_path: &'a str, meta: &'a CachedPath) -> Self {
        Self {
            store,
            store_path,
            meta,
        }
    }

    fn build_path_info(&self, meta: &CachedPath, nar_size: u64) -> Result<ValidPathInfo> {
        let path_base = self
            .store_path
            .strip_prefix("/nix/store/")
            .unwrap_or(self.store_path);

        let path = StorePath::from_base_path(path_base)
            .map_err(|e| anyhow::anyhow!("invalid store path {}: {}", self.store_path, e))?;

        let info = build_unkeyed_path_info(self.store_path, meta, nar_size)?;
        Ok(ValidPathInfo { path, info })
    }

    async fn import(&self, payload: NarPayload) -> Result<()> {
        let compressed_len = payload.byte_len().await;
        let nar = Arc::new(FetchedNar::new(payload, self.meta.url.clone()));
        self.verify_and_import(&nar).await?;
        debug!(%self.store_path, bytes = compressed_len, "imported NAR into local store");
        Ok(())
    }

    async fn verify_and_import(&self, nar: &Arc<FetchedNar>) -> Result<()> {
        let store_path = self.store_path.to_owned();
        let digesting = Arc::clone(nar);
        let digest = tokio::task::spawn_blocking(move || {
            digesting
                .digest()
                .with_context(|| format!("decompress failed for {store_path}"))
        })
        .await
        .context("decompress task panicked")??;
        verify_nar(
            self.store_path,
            &digest,
            self.meta.nar_size,
            self.meta.nar_hash.as_deref(),
        )?;
        let meta = with_nar_hash(self.meta, &digest);
        let valid_info = self.build_path_info(&meta, digest.size)?;

        let (reader, writer) = tokio::io::duplex(PIPE_BYTES);
        let source = Arc::clone(nar);
        let decompressing = tokio::task::spawn_blocking(move || {
            let mut bridge = tokio_util::io::SyncIoBridge::new(writer);
            std::io::copy(&mut source.decoder()?, &mut bridge).context("stream verified NAR")?;
            Ok::<_, anyhow::Error>(())
        });
        let imported = self
            .store
            .import_nar(&valid_info, tokio::io::BufReader::new(reader))
            .await;
        let decompressed = decompressing.await.context("decompress task panicked")?;

        if let (Err(_), Err(e)) = (&imported, &decompressed) {
            warn!(%self.store_path, error = format!("{e:#}"), "NAR stream ended with the import");
        }
        imported.and(decompressed)
    }
}

const PIPE_BYTES: usize = 1 << 20;
const READ_BYTES: usize = 1 << 16;

/// A fetched NAR is decompressed once per reader from its compressed form: to verify it, then
/// into the daemon or an upload. Decompressing it into memory held every NAR of every running job
/// in RAM at once.
pub(crate) struct FetchedNar {
    compressed: NarPayload,
    url: Option<String>,
}

impl FetchedNar {
    pub(crate) fn new(compressed: NarPayload, url: Option<String>) -> Self {
        Self { compressed, url }
    }

    /// Compression is detected from the payload magic bytes, then the narinfo `URL:` field, then
    /// zstd. A `NarRequest` over the WebSocket is only carrying zstd.
    fn decoder(&self) -> Result<Box<dyn std::io::Read + Send + '_>> {
        let mut body = self.compressed.reader().context("open staged NAR")?;
        let mut magic = Vec::with_capacity(SNIFF_BYTES);
        body.by_ref()
            .take(SNIFF_BYTES as u64)
            .read_to_end(&mut magic)
            .context("read staged NAR")?;
        let kind = resolve_compression(&magic, self.url.as_deref());
        decoder(std::io::Cursor::new(magic).chain(body), kind)
    }

    fn digest(&self) -> Result<NarDigest> {
        NarDigest::of(self.decoder()?).context("read compressed NAR")
    }
}

impl NarReader for FetchedNar {
    fn open(&self) -> Result<Box<dyn std::io::Read + Send + '_>> {
        self.decoder()
    }
}

/// A downloaded file as the NAR of that single regular file.
pub(crate) struct FlatFileNar {
    file: NarPayload,
    len: u64,
    executable: bool,
}

impl FlatFileNar {
    pub(crate) async fn new(file: NarPayload, executable: bool) -> Self {
        Self {
            len: file.byte_len().await,
            file,
            executable,
        }
    }

    pub(crate) async fn file_digest(self: &Arc<Self>) -> Result<NarDigest> {
        let flat = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            NarDigest::of(flat.file.reader()?).context("read downloaded file")
        })
        .await
        .context("digest task panicked")?
    }
}

impl NarReader for FlatFileNar {
    fn open(&self) -> Result<Box<dyn std::io::Read + Send + '_>> {
        let (head, tail) = gradient_util::nar::single_file_nar_frame(self.len, self.executable);
        Ok(Box::new(
            std::io::Cursor::new(head)
                .chain(self.file.reader()?)
                .chain(std::io::Cursor::new(tail)),
        ))
    }
}

pub(crate) async fn digest_of(nar: Arc<dyn NarReader>) -> Result<NarDigest> {
    tokio::task::spawn_blocking(move || NarDigest::of(nar.open()?).context("read NAR"))
        .await
        .context("digest task panicked")?
}

pub(crate) async fn holds_claimed_nar(compressed: &NarPayload, cp: &CachedPath) -> bool {
    let nar = FetchedNar::new(compressed.clone(), cp.url.clone());
    let nar_size = cp.nar_size;
    let claimed = cp
        .nar_hash
        .as_deref()
        .and_then(|hash| parse_nar_hash_to_bytes(hash).ok());
    tokio::task::spawn_blocking(move || {
        nar.digest().is_ok_and(|digest| {
            nar_size.is_none_or(|size| size == digest.size) && claimed == Some(digest.sha256)
        })
    })
    .await
    .unwrap_or(false)
}

pub(crate) struct NarDigest {
    pub(crate) size: u64,
    pub(crate) sha256: [u8; 32],
}

impl NarDigest {
    fn of(mut nar: impl std::io::Read) -> std::io::Result<Self> {
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        let mut buf = vec![0u8; READ_BYTES];
        loop {
            match nar.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    hasher.update(&buf[..n]);
                    size += n as u64;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }

        Ok(Self {
            size,
            sha256: hasher.finalize().into(),
        })
    }

    pub(crate) fn nix32(&self) -> String {
        gradient_worker_client::nar::sha256_digest_nix32(&self.sha256)
    }
}

fn with_nar_hash(meta: &CachedPath, nar: &NarDigest) -> CachedPath {
    let mut meta = meta.clone();
    meta.nar_hash.get_or_insert_with(|| nar.nix32());
    meta
}

/// A mismatch is a typed [`CorruptCachedNar`]. The executor is routing it into demote-and-refetch
/// instead of a retry loop.
fn verify_nar(
    store_path: &str,
    nar: &NarDigest,
    expected_size: Option<u64>,
    claimed_nar_hash: Option<&str>,
) -> Result<()> {
    if let Some(expected) = expected_size
        && nar.size != expected
    {
        return Err(
            anyhow::Error::new(CorruptCachedNar(store_path.to_owned())).context(format!(
                "NAR size mismatch for {}: expected {}, got {}",
                store_path, expected, nar.size
            )),
        );
    }

    if let Some(claimed_nar_hash) = claimed_nar_hash {
        let claimed = parse_nar_hash_to_bytes(claimed_nar_hash)
            .with_context(|| format!("invalid nar_hash for {store_path}"))?;

        if nar.sha256 != claimed {
            return Err(
                anyhow::Error::new(CorruptCachedNar(store_path.to_owned())).context(format!(
                    "NAR hash mismatch for {}: server said {}, computed {}",
                    store_path,
                    claimed_nar_hash,
                    nar.nix32()
                )),
            );
        }
    }

    Ok(())
}

pub async fn import_received_nar(
    store: &LocalNixStore,
    store_path: &str,
    payload: NarPayload,
    meta: &CachedPath,
) -> Result<()> {
    NarImporter::new(store, store_path, meta)
        .import(payload)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nar_without_a_claimed_hash_is_registered_under_the_hash_of_its_bytes() {
        let meta = CachedPath {
            path: "/nix/store/aaaa-hello".into(),
            ..CachedPath::default()
        };
        let filled = with_nar_hash(&meta, &NarDigest::of(&b"nar bytes"[..]).unwrap());
        assert_eq!(
            filled.nar_hash.as_deref(),
            Some(gradient_worker_client::nar::sha256_nix32(b"nar bytes").as_str())
        );
    }

    #[test]
    fn a_claimed_hash_is_kept() {
        let meta = CachedPath {
            nar_hash: Some("sha256:claimed".into()),
            ..CachedPath::default()
        };
        assert_eq!(
            with_nar_hash(&meta, &NarDigest::of(&b"nar bytes"[..]).unwrap())
                .nar_hash
                .as_deref(),
            Some("sha256:claimed")
        );
    }

    fn fetched(raw: &[u8]) -> (FetchedNar, NarDigest) {
        let nar = FetchedNar::new(NarPayload::from(zstd::encode_all(raw, 0).unwrap()), None);
        let digest = nar.digest().unwrap();
        (nar, digest)
    }

    #[tokio::test]
    async fn a_flat_file_reads_as_the_nar_of_that_file() {
        for (contents, executable) in [
            (&b"hi\n"[..], false),
            (&b"12345678"[..], true),
            (&b""[..], false),
        ] {
            let flat = FlatFileNar::new(NarPayload::from(contents.to_vec()), executable).await;
            let mut nar = Vec::new();
            flat.open().unwrap().read_to_end(&mut nar).unwrap();
            assert_eq!(
                nar,
                gradient_util::nar::single_file_nar(contents, executable)
            );
        }
    }

    #[tokio::test]
    async fn a_body_is_vouched_for_by_the_claimed_hash_and_size_of_its_nar() {
        let raw = b"nix-archive-1 body".to_vec();
        let body = || NarPayload::from(zstd::encode_all(raw.as_slice(), 0).unwrap());
        let claimed = CachedPath {
            nar_size: Some(raw.len() as u64),
            nar_hash: Some(gradient_worker_client::nar::sha256_nix32(&raw)),
            ..CachedPath::default()
        };
        assert!(holds_claimed_nar(&body(), &claimed).await);

        let other = CachedPath {
            nar_hash: Some(gradient_worker_client::nar::sha256_nix32(b"other")),
            ..claimed.clone()
        };
        assert!(!holds_claimed_nar(&body(), &other).await);

        let unclaimed = CachedPath {
            nar_hash: None,
            ..claimed
        };
        assert!(!holds_claimed_nar(&body(), &unclaimed).await);
    }

    #[test]
    fn a_compressed_nar_is_verified_against_the_size_and_hash_of_its_content() {
        let raw = vec![7u8; 3 * READ_BYTES + 5];
        let (_, digest) = fetched(&raw);
        let claimed = gradient_worker_client::nar::sha256_nix32(&raw);

        verify_nar(
            "/nix/store/aaaa-hello",
            &digest,
            Some(raw.len() as u64),
            Some(&claimed),
        )
        .unwrap();

        let short = verify_nar("/nix/store/aaaa-hello", &digest, Some(1), Some(&claimed));
        assert!(short.unwrap_err().is::<CorruptCachedNar>());

        let other = gradient_worker_client::nar::sha256_nix32(b"other");
        let swapped = verify_nar("/nix/store/aaaa-hello", &digest, None, Some(&other));
        assert!(swapped.unwrap_err().is::<CorruptCachedNar>());
    }

    #[test]
    fn a_fetched_nar_decompresses_to_the_same_content_on_every_pass() {
        let raw: Vec<u8> = (0..200_000u32).flat_map(u32::to_le_bytes).collect();
        let (nar, digest) = fetched(&raw);

        let mut streamed = Vec::new();
        nar.decoder().unwrap().read_to_end(&mut streamed).unwrap();
        assert_eq!(streamed, raw);
        assert_eq!(digest.size, raw.len() as u64);
        assert_eq!(nar.digest().unwrap().sha256, digest.sha256);
    }
}
