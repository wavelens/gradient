/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! NAR compression handling: format detection, decompression, `.drv`
//! closure-seed extraction, and `ValidPathInfo` construction shared by the
//! prefetch, substitute-relay, and daemon-import paths.

use std::collections::BTreeSet;
use std::io::Read as _;

use anyhow::{Context, Result};
use gradient_db::parse_drv;
use gradient_wire::messages::CachedPath;
use harmonia_protocol::valid_path_info::UnkeyedValidPathInfo;
use harmonia_store_path::{StoreDir, StorePath};
use harmonia_utils_hash::fmt::Any;
use harmonia_utils_hash::{Hash, HashView as _};
use harmonia_utils_signature::Signature;
use tracing::warn;

/// Compression format for a NAR as declared by the cache it came from.
/// Identified by filename extension on the `URL:` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Compression {
    None,
    Zstd,
    Xz,
    Bzip2,
}

/// Infer a NAR's compression format from the URL extension. Unknown or
/// missing extension → `Zstd`, since our own cache always produces zstd;
/// this keeps the `NarRequest` / S3 path correct while letting upstream
/// URLs like `.nar.xz` dispatch accordingly.
pub(crate) fn detect_compression(url: &str) -> Compression {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".nar.xz") || lower.ends_with(".xz") {
        Compression::Xz
    } else if lower.ends_with(".nar.bz2") || lower.ends_with(".bz2") {
        Compression::Bzip2
    } else if lower.ends_with(".nar.zst") || lower.ends_with(".zst") {
        Compression::Zstd
    } else if lower.ends_with(".nar") {
        Compression::None
    } else {
        Compression::Zstd
    }
}

/// Serialised `nix-archive-1` token every uncompressed NAR opens with: an
/// 8-byte little-endian length followed by the string itself.
const NAR_MAGIC: &[u8] = b"\x0d\x00\x00\x00\x00\x00\x00\x00nix-archive-1";

/// Leading bytes a caller must hand [`sniff_compression`] for it to be able to
/// identify every container: the uncompressed NAR header is the longest.
pub(crate) const SNIFF_BYTES: usize = NAR_MAGIC.len();

/// Identify a NAR payload's container from its leading magic bytes, or `None`
/// when nothing matches (a body too short to classify, or a format we don't
/// handle).
pub(crate) fn sniff_compression(bytes: &[u8]) -> Option<Compression> {
    if bytes.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        Some(Compression::Zstd)
    } else if bytes.starts_with(&[0xFD, b'7', b'z', b'X', b'Z', 0x00]) {
        Some(Compression::Xz)
    } else if bytes.starts_with(b"BZh") {
        Some(Compression::Bzip2)
    } else if bytes.starts_with(NAR_MAGIC) {
        Some(Compression::None)
    } else {
        None
    }
}

/// The format to decompress `bytes` as: what the bytes actually are, falling
/// back to [`detect_compression`]'s URL guess when they carry no known magic.
///
/// The bytes win because caches disagree with their own metadata: attic serves
/// `Compression: zstd` under a `nar/<hash>.nar` URL, so the extension alone
/// hands a zstd frame to the daemon as a raw NAR and the import fails on a size
/// mismatch that looks like cache corruption.
pub(crate) fn resolve_compression(bytes: &[u8], url: Option<&str>) -> Compression {
    sniff_compression(bytes)
        .unwrap_or_else(|| url.map(detect_compression).unwrap_or(Compression::Zstd))
}

/// Decompress a NAR payload per its compression format. Synchronous; NAR
/// payloads are bounded by `nar_size` from the path info, so memory
/// pressure is predictable.
pub(crate) fn decompress(compressed: &[u8], kind: Compression) -> Result<Vec<u8>> {
    decompress_reader(std::io::Cursor::new(compressed), kind)
}

/// Decompress a NAR payload straight from `reader`, so a NAR staged on disk is
/// never held in memory in its compressed form as well as its raw one.
pub(crate) fn decompress_reader<R: std::io::Read>(reader: R, kind: Compression) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    match kind {
        Compression::None => {
            let mut reader = reader;
            reader.read_to_end(&mut out).context("read raw NAR")?;
        }
        Compression::Zstd => {
            let mut decoder = zstd::stream::Decoder::new(reader).context("init zstd decoder")?;
            decoder.read_to_end(&mut out).context("read zstd stream")?;
        }
        Compression::Xz => {
            let mut decoder = xz2::read::XzDecoder::new(reader);
            decoder.read_to_end(&mut out).context("read xz stream")?;
        }
        Compression::Bzip2 => {
            let mut decoder = bzip2::read::BzDecoder::new(reader);
            decoder.read_to_end(&mut out).context("read bzip2 stream")?;
        }
    }
    Ok(out)
}

/// Extract the single regular-file payload from a NAR. `.drv` files are
/// stored as exactly that, so this is enough to recover the .drv bytes
/// without writing them to disk first.
pub(crate) async fn extract_single_file_from_nar(nar_bytes: &[u8]) -> Result<Vec<u8>> {
    use futures::StreamExt as _;
    use harmonia_file_nar::{NarEvent, parse_nar};
    use tokio::io::AsyncReadExt as _;

    let cursor = std::io::Cursor::new(nar_bytes.to_vec());
    let mut stream = std::pin::pin!(parse_nar(cursor));
    let event = stream
        .next()
        .await
        .ok_or_else(|| anyhow::anyhow!("NAR is empty"))??;
    match event {
        NarEvent::File { mut reader, .. } => {
            let mut buf = Vec::new();
            reader
                .read_to_end(&mut buf)
                .await
                .context("read NAR file body")?;
            Ok(buf)
        }
        _ => Err(anyhow::anyhow!("expected single regular file in NAR")),
    }
}

/// Every nix-store path a `.drv` lets us reach when expanding the prefetch
/// closure: its input derivations (the `.drv` files this one depends on) and
/// input sources (plain files the daemon validates when accepting the `.drv`
/// NAR). Its outputs are not seeds: which of an input's outputs a build needs
/// is decided by the consumer's requested output names.
///
/// We re-derive these from the `.drv` content rather than relying solely on
/// `cached_path.references` because the eval worker can silently store a
/// `NULL` references column when its `gather_path_meta` query fails -
/// without this fallback the daemon then rejects the `.drv` import with
/// `path '…' is not valid` for a reference parsed straight out of the
/// `.drv` text.
pub(crate) fn drv_closure_seeds(drv: &gradient_db::Derivation) -> Vec<String> {
    drv.input_derivations
        .iter()
        .map(|(drv_path, _)| drv_path)
        .chain(&drv.input_sources)
        .cloned()
        .collect()
}

/// Decompress a `.drv`'s NAR, parse it, and return the closure-walk seeds
/// (see [`drv_closure_seeds`]). Returns an empty vec on any failure - the
/// caller proceeds with what it has so a transient parse problem does not
/// stall the closure walk.
pub(crate) async fn drv_closure_seeds_from_compressed_nar(
    compressed: &[u8],
    compression: Compression,
    drv_path: &str,
) -> Vec<String> {
    let owned = compressed.to_vec();
    let nar = match tokio::task::spawn_blocking(move || decompress(&owned, compression)).await {
        Ok(Ok(b)) => b,
        Ok(Err(e)) => {
            warn!(drv = %drv_path, error = %e, "decompress failed while harvesting drv closure seeds");
            return Vec::new();
        }
        Err(e) => {
            warn!(drv = %drv_path, error = %e, "decompress task panicked while harvesting drv closure seeds");
            return Vec::new();
        }
    };
    let drv_bytes = match extract_single_file_from_nar(&nar).await {
        Ok(b) => b,
        Err(e) => {
            warn!(drv = %drv_path, error = %e, "could not extract drv file from NAR");
            return Vec::new();
        }
    };
    let drv = match parse_drv(&drv_bytes) {
        Ok(d) => d,
        Err(e) => {
            warn!(drv = %drv_path, error = %e, "could not parse fetched .drv");
            return Vec::new();
        }
    };
    drv_closure_seeds(&drv)
}

/// Parse a `sha256:<...>` (or `sha256-<base64>` SRI) hash into the raw 32-byte
/// digest expected for byte-wise comparison against `Sha256::digest`.
pub(crate) fn parse_nar_hash_to_bytes(s: &str) -> Result<[u8; 32]> {
    let hash_any = s
        .parse::<Any<Hash>>()
        .map_err(|e| anyhow::anyhow!("parse hash {}: {}", s, e))?;

    let hash: Hash = hash_any.into_hash();
    let bytes = hash.digest_bytes();
    if bytes.len() != 32 {
        anyhow::bail!("expected 32-byte SHA-256 digest, got {}", bytes.len());
    }

    let mut out = [0u8; 32];
    out.copy_from_slice(bytes);
    Ok(out)
}

/// Build the `UnkeyedValidPathInfo` for `add_to_store_nar` from the cache
/// metadata. Falls back to a default `ca = None` / `deriver = None` /
/// `signatures = {}` when the server didn't supply them.
pub(crate) fn build_unkeyed_path_info(
    store_path: &str,
    meta: &CachedPath,
    nar_size: u64,
) -> Result<UnkeyedValidPathInfo> {
    let nar_hash_str = meta
        .nar_hash
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("cache metadata missing nar_hash for {}", store_path))?;

    let hash_any = nar_hash_str
        .parse::<Any<Hash>>()
        .map_err(|e| anyhow::anyhow!("parse nar_hash '{}': {}", nar_hash_str, e))?;

    let nar_hash = hash_any
        .into_hash()
        .try_into()
        .map_err(|e| anyhow::anyhow!("convert nar_hash '{}' to NarHash: {}", nar_hash_str, e))?;

    let mut references: BTreeSet<StorePath> = BTreeSet::new();
    if let Some(refs) = meta.references.as_ref() {
        for r in refs {
            let base = r.strip_prefix("/nix/store/").unwrap_or(r);
            match StorePath::from_base_path(base) {
                Ok(sp) => {
                    references.insert(sp);
                }
                Err(e) => {
                    warn!(reference = %r, error = %e, "skipping invalid reference");
                }
            }
        }
    }

    let deriver = meta.deriver.as_ref().and_then(|d| {
        let base = d.strip_prefix("/nix/store/").unwrap_or(d);
        match StorePath::from_base_path(base) {
            Ok(sp) => Some(sp),
            Err(e) => {
                warn!(deriver = %d, error = %e, "skipping invalid deriver");
                None
            }
        }
    });

    let mut signatures: BTreeSet<Signature> = BTreeSet::new();
    if let Some(sigs) = meta.signatures.as_ref() {
        for s in sigs {
            match s.parse::<Signature>() {
                Ok(sig) => {
                    signatures.insert(sig);
                }
                Err(e) => {
                    warn!(signature = %s, error = %e, "skipping unparseable signature");
                }
            }
        }
    }

    let ca = meta.ca.as_ref().and_then(|c| match c.parse() {
        Ok(parsed) => Some(parsed),
        Err(_) => {
            warn!(ca = %c, "skipping unparseable content-address");
            None
        }
    });

    Ok(UnkeyedValidPathInfo {
        deriver,
        nar_hash,
        references,
        registration_time: None,
        nar_size,
        ultimate: false,
        signatures,
        ca,
        store_dir: StoreDir::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_compression_from_url_extensions() {
        assert_eq!(
            detect_compression("https://cache.nixos.org/nar/abc.nar.xz"),
            Compression::Xz
        );
        assert_eq!(
            detect_compression("https://cache.example/nar/abc.nar.bz2"),
            Compression::Bzip2
        );
        assert_eq!(
            detect_compression("https://cache.example/nar/abc.nar.zst"),
            Compression::Zstd
        );
        assert_eq!(
            detect_compression("https://cache.example/nar/abc.nar"),
            Compression::None
        );
        // S3 presigned URLs carry a query string - must not confuse the matcher.
        assert_eq!(
            detect_compression("https://s3.example/abc.nar.xz?sig=XYZ&exp=1"),
            Compression::Xz
        );
        // Unknown / no extension defaults to zstd (our own cache).
        assert_eq!(
            detect_compression("https://example/some/opaque"),
            Compression::Zstd
        );
    }

    /// Regression for the attic upstream that advertises `Compression: zstd`
    /// behind a `nar/<hash>.nar` URL: the extension says "no compression" while
    /// the bytes are a zstd frame. Sniffing the magic must win over the guess.
    #[test]
    fn sniff_wins_over_a_lying_url_extension() {
        let payload = b"hello gradient zstd world";
        let compressed = zstd::stream::encode_all(&payload[..], 6).unwrap();
        assert_eq!(sniff_compression(&compressed), Some(Compression::Zstd));
        assert_eq!(
            resolve_compression(&compressed, Some("https://attic.example/nar/abc.nar")),
            Compression::Zstd
        );
        let out = decompress(
            &compressed,
            resolve_compression(&compressed, Some("https://attic.example/nar/abc.nar")),
        )
        .unwrap();
        assert_eq!(out, payload);
    }

    #[test]
    fn sniff_recognises_an_uncompressed_nar() {
        let mut nar = NAR_MAGIC.to_vec();
        nar.extend_from_slice(&[0u8; 3]);
        assert_eq!(sniff_compression(&nar), Some(Compression::None));
        // …even when the URL claims zstd.
        assert_eq!(
            resolve_compression(&nar, Some("https://cache.example/nar/abc.nar.zst")),
            Compression::None
        );
    }

    #[test]
    fn sniff_recognises_xz_and_bzip2() {
        use std::io::Write;
        let mut xz = xz2::write::XzEncoder::new(Vec::new(), 6);
        xz.write_all(b"payload").unwrap();
        assert_eq!(
            sniff_compression(&xz.finish().unwrap()),
            Some(Compression::Xz)
        );

        let mut bz = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
        bz.write_all(b"payload").unwrap();
        assert_eq!(
            sniff_compression(&bz.finish().unwrap()),
            Some(Compression::Bzip2)
        );
    }

    /// Unrecognised bytes (and a body too short to carry any magic) must fall
    /// back to the URL guess rather than assert a format.
    #[test]
    fn sniff_falls_back_to_the_url_when_no_magic_matches() {
        assert_eq!(sniff_compression(b"not a known container"), None);
        assert_eq!(sniff_compression(b""), None);
        assert_eq!(
            resolve_compression(b"opaque", Some("https://cache.example/nar/abc.nar.xz")),
            Compression::Xz
        );
        // No URL at all: our own cache only ever produces zstd.
        assert_eq!(resolve_compression(b"opaque", None), Compression::Zstd);
    }

    #[test]
    fn decompress_none_passthrough() {
        let raw = b"raw NAR bytes".to_vec();
        let out = decompress(&raw, Compression::None).unwrap();
        assert_eq!(out, raw);
    }

    /// The staged-file import path decompresses through a reader; it must agree
    /// byte for byte with the in-memory path the presigned download still uses.
    #[test]
    fn decompress_reader_matches_decompress_for_zstd() {
        let payload = b"gradient staged nar payload".repeat(64);
        let compressed = zstd::encode_all(std::io::Cursor::new(&payload), 6).unwrap();
        let from_slice = decompress(&compressed, Compression::Zstd).unwrap();
        let from_reader =
            decompress_reader(std::io::Cursor::new(&compressed), Compression::Zstd).unwrap();
        assert_eq!(from_slice, payload);
        assert_eq!(from_reader, from_slice);
    }

    #[test]
    fn decompress_roundtrip_xz() {
        use std::io::Write;
        let payload = b"hello gradient xz world";
        let mut encoder = xz2::write::XzEncoder::new(Vec::new(), 6);
        encoder.write_all(payload).unwrap();
        let compressed = encoder.finish().unwrap();
        let out = decompress(&compressed, Compression::Xz).unwrap();
        assert_eq!(out, payload);
    }

    #[test]
    fn decompress_roundtrip_bzip2() {
        use std::io::Write;
        let payload = b"hello gradient bzip2 world";
        let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
        encoder.write_all(payload).unwrap();
        let compressed = encoder.finish().unwrap();
        let out = decompress(&compressed, Compression::Bzip2).unwrap();
        assert_eq!(out, payload);
    }

    #[test]
    fn parse_sha256_nix32_roundtrip() {
        use sha2::{Digest as _, Sha256};
        // SHA-256 of the empty string in nix32 form.
        let nix32 = "sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73";
        let bytes = parse_nar_hash_to_bytes(nix32).unwrap();
        let expected: [u8; 32] = Sha256::digest(b"").into();
        assert_eq!(bytes, expected);
    }

    #[test]
    fn build_unkeyed_minimal_meta() {
        let meta = CachedPath {
            path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-x".into(),
            cached: true,
            file_size: None,
            nar_size: Some(123),
            url: None,
            multipart: None,
            nar_hash: Some("sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73".into()),
            file_hash: None,
            references: None,
            signatures: None,
            deriver: None,
            ca: None,
        };
        let info = build_unkeyed_path_info(&meta.path, &meta, 123).unwrap();
        assert_eq!(info.nar_size, 123);
        assert!(info.references.is_empty());
        assert!(info.signatures.is_empty());
        assert!(info.deriver.is_none());
        assert!(info.ca.is_none());
        assert!(!info.ultimate);
    }

    #[test]
    fn build_unkeyed_collects_references_and_signatures() {
        // Nix store path hashes are exactly 32 chars in nix32 (160 bits).
        let meta = CachedPath {
            path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-x".into(),
            cached: true,
            file_size: None,
            nar_size: Some(0),
            url: None,
            multipart: None,
            nar_hash: Some("sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73".into()),
            file_hash: None,
            references: Some(vec![
                "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-y".into(),
                "/nix/store/cccccccccccccccccccccccccccccccc-z".into(),
            ]),
            signatures: Some(vec![
                // Both malformed (Ed25519 sigs are 88 base64 chars); should be
                // dropped without aborting the path-info construction.
                "cache.example.com-1:tooShort".into(),
                "garbage-no-colon".into(),
            ]),
            deriver: Some("/nix/store/dddddddddddddddddddddddddddddddd-x.drv".into()),
            ca: None,
        };
        let info = build_unkeyed_path_info(&meta.path, &meta, 0).unwrap();
        assert_eq!(info.references.len(), 2);
        assert!(info.deriver.is_some());
        // Both signatures were malformed and should have been skipped.
        assert_eq!(info.signatures.len(), 0);
    }

    #[test]
    fn missing_nar_hash_is_an_error() {
        let meta = CachedPath {
            path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-x".into(),
            cached: true,
            file_size: None,
            nar_size: Some(0),
            url: None,
            multipart: None,
            nar_hash: None,
            file_hash: None,
            references: None,
            signatures: None,
            deriver: None,
            ca: None,
        };
        assert!(build_unkeyed_path_info(&meta.path, &meta, 0).is_err());
    }

    /// A `.drv`'s closure seeds are its inputs (input_derivations +
    /// input_sources), never its outputs. The daemon validates exactly those
    /// references when importing the `.drv`, and an input's outputs are
    /// chosen by the consumer's requested output names, not by the walk.
    #[test]
    fn drv_closure_seeds_are_inputs_never_outputs() {
        use gradient_db::parse_drv;

        let drv_bytes = br#"Derive([("debug","/nix/store/dddddddddddddddddddddddddddddddd-debug","",""),("out","/nix/store/oooooooooooooooooooooooooooooooo-out","","")],[("/nix/store/iiiiiiiiiiiiiiiiiiiiiiiiiiiiiiii-dep.drv",["out"])],["/nix/store/ssssssssssssssssssssssssssssssss-src.sh"],"x86_64-linux","/bin/sh",[],[])"#;
        let drv = parse_drv(drv_bytes).unwrap();

        assert_eq!(
            drv_closure_seeds(&drv),
            vec![
                "/nix/store/iiiiiiiiiiiiiiiiiiiiiiiiiiiiiiii-dep.drv".to_string(),
                "/nix/store/ssssssssssssssssssssssssssssssss-src.sh".to_string(),
            ]
        );
    }
}
