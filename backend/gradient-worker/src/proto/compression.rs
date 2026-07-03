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
use gradient_proto::messages::CachedPath;
use harmonia_protocol::valid_path_info::UnkeyedValidPathInfo;
use harmonia_store_path::{StoreDir, StorePath};
use harmonia_utils_hash::fmt::Any;
use harmonia_utils_hash::{Hash, HashView as _};
use harmonia_utils_signature::Signature;
use tracing::warn;

use crate::proto::prefetch::ClosureMode;

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

/// Decompress a NAR payload per its compression format. Synchronous; NAR
/// payloads are bounded by `nar_size` from the path info, so memory
/// pressure is predictable.
pub(crate) fn decompress(compressed: &[u8], kind: Compression) -> Result<Vec<u8>> {
    match kind {
        Compression::None => Ok(compressed.to_vec()),
        Compression::Zstd => decompress_zstd(compressed),
        Compression::Xz => decompress_xz(compressed),
        Compression::Bzip2 => decompress_bzip2(compressed),
    }
}

/// zstd window size produced by compression level 6 (`windowLog` 21 = 2 MiB).
/// An upstream zstd NAR with a window at least this large is relayed verbatim;
/// a smaller window (levels 1-2) is recompressed at level 6 before storing.
pub(crate) const LEVEL6_WINDOW_BYTES: u64 = 2 * 1024 * 1024;

/// Read the `Window_Size` encoded in a zstd frame header (RFC 8878 §3.1.1.1).
/// Returns `None` when `frame` isn't a zstd frame or its header is truncated.
/// For single-segment frames (no `Window_Descriptor`) the window equals the
/// `Frame_Content_Size`.
pub(crate) fn zstd_window_size(frame: &[u8]) -> Option<u64> {
    if frame.len() < 5 || frame[0..4] != [0x28, 0xB5, 0x2F, 0xFD] {
        return None;
    }

    let descriptor = frame[4];
    let fcs_flag = (descriptor >> 6) as usize;
    let single_segment = descriptor & 0x20 != 0;
    let dict_id_flag = (descriptor & 0x03) as usize;

    if !single_segment {
        let wd = *frame.get(5)?;
        let exponent = u32::from(wd >> 3);
        let mantissa = u64::from(wd & 0x7);
        let window_base = 1u64.checked_shl(10 + exponent)?;

        return Some(window_base + (window_base / 8) * mantissa);
    }

    // Single-segment: Window_Size == Frame_Content_Size, located after the
    // optional Dictionary_ID. FCS_flag 0 is a single byte in this mode.
    let dict_size = [0usize, 1, 2, 4][dict_id_flag];
    let fcs_size = [1usize, 2, 4, 8][fcs_flag];
    let start = 5 + dict_size;
    let bytes = frame.get(start..start + fcs_size)?;

    let mut fcs = bytes
        .iter()
        .enumerate()
        .fold(0u64, |acc, (i, b)| acc | (u64::from(*b) << (8 * i)));
    if fcs_flag == 1 {
        fcs += 256;
    }

    Some(fcs)
}

/// Extract the single regular-file payload from a NAR. `.drv` files are
/// stored as exactly that, so this is enough to recover the .drv bytes
/// without writing them to disk first.
async fn extract_single_file_from_nar(nar_bytes: &[u8]) -> Result<Vec<u8>> {
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
/// closure: under [`ClosureMode::FollowOutputs`] this includes declared
/// outputs (so downstream builds find them), input derivations (the `.drv`
/// files this one depends on), and input sources (plain files the daemon
/// will validate when accepting the `.drv` NAR). Under
/// [`ClosureMode::InputsOnly`] the outputs are omitted - used when fetching
/// a build target's own `.drv`, whose outputs aren't yet in the cache.
///
/// We re-derive these from the `.drv` content rather than relying solely on
/// `cached_path.references` because the eval worker can silently store a
/// `NULL` references column when its `gather_path_meta` query fails -
/// without this fallback the daemon then rejects the `.drv` import with
/// `path '…' is not valid` for a reference parsed straight out of the
/// `.drv` text.
pub(crate) fn drv_closure_seeds(drv: &gradient_db::Derivation, mode: ClosureMode) -> Vec<String> {
    let mut out = Vec::with_capacity(
        drv.outputs.len() + drv.input_derivations.len() + drv.input_sources.len(),
    );
    if matches!(mode, ClosureMode::FollowOutputs) {
        for o in &drv.outputs {
            if !o.path.is_empty() {
                out.push(o.path.clone());
            }
        }
    }
    for (drv_path, _) in &drv.input_derivations {
        out.push(drv_path.clone());
    }
    for src in &drv.input_sources {
        out.push(src.clone());
    }
    out
}

/// Decompress a `.drv`'s NAR, parse it, and return the closure-walk seeds
/// (see [`drv_closure_seeds`]). Returns an empty vec on any failure - the
/// caller proceeds with what it has so a transient parse problem does not
/// stall the closure walk.
pub(crate) async fn drv_closure_seeds_from_compressed_nar(
    compressed: &[u8],
    compression: Compression,
    drv_path: &str,
    mode: ClosureMode,
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
    drv_closure_seeds(&drv, mode)
}

fn decompress_zstd(compressed: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = zstd::stream::Decoder::new(std::io::Cursor::new(compressed))
        .context("init zstd decoder")?;

    let mut out = Vec::with_capacity(compressed.len() * 4);
    decoder.read_to_end(&mut out).context("read zstd stream")?;

    Ok(out)
}

fn decompress_xz(compressed: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = xz2::read::XzDecoder::new(std::io::Cursor::new(compressed));
    let mut out = Vec::with_capacity(compressed.len() * 4);
    decoder.read_to_end(&mut out).context("read xz stream")?;
    Ok(out)
}

fn decompress_bzip2(compressed: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = bzip2::read::BzDecoder::new(std::io::Cursor::new(compressed));
    let mut out = Vec::with_capacity(compressed.len() * 4);
    decoder.read_to_end(&mut out).context("read bzip2 stream")?;
    Ok(out)
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

    #[test]
    fn decompress_none_passthrough() {
        let raw = b"raw NAR bytes".to_vec();
        let out = decompress(&raw, Compression::None).unwrap();
        assert_eq!(out, raw);
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

    /// Build a minimal multi-segment zstd frame header (magic + descriptor +
    /// `Window_Descriptor`) encoding `window_log` with the given `mantissa`.
    fn zstd_header(window_log: u8, mantissa: u8) -> Vec<u8> {
        let wd = ((window_log - 10) << 3) | (mantissa & 0x7);
        vec![0x28, 0xB5, 0x2F, 0xFD, 0x00, wd]
    }

    #[test]
    fn zstd_window_size_decodes_window_descriptor() {
        // windowLog 21 (level 6) == exactly 2 MiB.
        assert_eq!(zstd_window_size(&zstd_header(21, 0)), Some(2 * 1024 * 1024));
        // windowLog 20 (level 2) is below the threshold.
        assert_eq!(zstd_window_size(&zstd_header(20, 0)), Some(1024 * 1024));
        // Mantissa adds windowBase/8 per unit.
        assert_eq!(
            zstd_window_size(&zstd_header(21, 4)),
            Some(2 * 1024 * 1024 + (2 * 1024 * 1024 / 8) * 4)
        );
    }

    #[test]
    fn zstd_window_size_rejects_non_zstd_and_truncated() {
        assert_eq!(zstd_window_size(b"not a zstd frame at all"), None);
        assert_eq!(zstd_window_size(&[0x28, 0xB5, 0x2F, 0xFD]), None); // no descriptor
        assert_eq!(zstd_window_size(&[0x28, 0xB5, 0x2F, 0xFD, 0x00]), None); // no window byte
    }

    #[test]
    fn zstd_window_size_matches_level6_threshold() {
        // A real level-6 frame over >2 MiB of data carries a >= 2 MiB window;
        // a level-1 frame over the same data stays below it. This anchors the
        // LEVEL6_WINDOW_BYTES assumption end-to-end.
        let data: Vec<u8> = (0..3 * 1024 * 1024).map(|i| (i % 251) as u8).collect();

        let level6 = zstd::encode_all(std::io::Cursor::new(&data), 6).unwrap();
        assert!(zstd_window_size(&level6).unwrap() >= LEVEL6_WINDOW_BYTES);

        let level1 = zstd::encode_all(std::io::Cursor::new(&data), 1).unwrap();
        assert!(zstd_window_size(&level1).unwrap() < LEVEL6_WINDOW_BYTES);
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
            nar_hash: None,
            file_hash: None,
            references: None,
            signatures: None,
            deriver: None,
            ca: None,
        };
        assert!(build_unkeyed_path_info(&meta.path, &meta, 0).is_err());
    }

    /// A `.drv`'s closure seeds must include its declared outputs *and* its
    /// inputs (input_derivations + input_sources) under
    /// [`ClosureMode::FollowOutputs`]. The prefetch closure walk relies on
    /// this so that when a server-supplied `cached_path.references` row is
    /// `NULL` or stale, the daemon doesn't reject the eventual
    /// `add_to_store_nar` with `path '…' is not valid` for a reference parsed
    /// out of the `.drv` content.
    #[test]
    fn drv_closure_seeds_include_outputs_inputs_and_sources() {
        use gradient_db::parse_drv;

        let drv_bytes = br#"Derive([("out","/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-out","","")],[("/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-dep.drv",["out"])],["/nix/store/cccccccccccccccccccccccccccccccc-src.sh"],"x86_64-linux","/nix/store/dddddddddddddddddddddddddddddddd-bash",[],[])"#;
        let drv = parse_drv(drv_bytes).unwrap();
        let seeds = drv_closure_seeds(&drv, ClosureMode::FollowOutputs);

        assert!(
            seeds.contains(&"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-out".to_string()),
            "output path missing from seeds: {seeds:?}"
        );
        assert!(
            seeds.contains(&"/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-dep.drv".to_string()),
            "input_derivation path missing from seeds: {seeds:?}"
        );
        assert!(
            seeds.contains(&"/nix/store/cccccccccccccccccccccccccccccccc-src.sh".to_string()),
            "input_source path missing from seeds: {seeds:?}"
        );
    }

    /// Regression: under [`ClosureMode::InputsOnly`] - used when fetching the
    /// build target's own `.drv` - declared output paths must be excluded
    /// from the closure walk. Including them would force the next
    /// `CacheQuery Pull` to request paths the gradient cache doesn't have
    /// (they're what we're about to build), classifying them `Uncached` and
    /// aborting the whole prefetch with a spurious "server cannot serve
    /// required inputs" error. Was the root cause of cross-worker imports
    /// failing with `daemon add_to_store_nar … store path '…' does not exist`
    /// for the build target's input_derivation `.drv`.
    #[test]
    fn drv_closure_seeds_inputs_only_excludes_outputs() {
        use gradient_db::parse_drv;

        let drv_bytes = br#"Derive([("out","/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-out","","")],[("/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-dep.drv",["out"])],["/nix/store/cccccccccccccccccccccccccccccccc-src.sh"],"x86_64-linux","/nix/store/dddddddddddddddddddddddddddddddd-bash",[],[])"#;
        let drv = parse_drv(drv_bytes).unwrap();
        let seeds = drv_closure_seeds(&drv, ClosureMode::InputsOnly);

        assert!(
            !seeds.contains(&"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-out".to_string()),
            "output path must NOT appear under InputsOnly: {seeds:?}"
        );
        assert!(
            seeds.contains(&"/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-dep.drv".to_string()),
            "input_derivation path missing from InputsOnly seeds: {seeds:?}"
        );
        assert!(
            seeds.contains(&"/nix/store/cccccccccccccccccccccccccccccccc-src.sh".to_string()),
            "input_source path missing from InputsOnly seeds: {seeds:?}"
        );
    }

    /// Content-addressed and "deferred" outputs are stored with an empty path
    /// in the `.drv`. The closure walk must skip them - feeding an empty
    /// string into the cache query produces a confusing "invalid store path"
    /// failure several stages downstream.
    #[test]
    fn drv_closure_seeds_skip_empty_output_paths() {
        use gradient_db::parse_drv;

        let drv_bytes = br#"Derive([("out","","r:sha256","deadbeef")],[],["/nix/store/cccccccccccccccccccccccccccccccc-src"],"x86_64-linux","/nix/store/dddddddddddddddddddddddddddddddd-bash",[],[])"#;
        let drv = parse_drv(drv_bytes).unwrap();
        let seeds = drv_closure_seeds(&drv, ClosureMode::FollowOutputs);

        assert!(
            !seeds.iter().any(|s| s.is_empty()),
            "empty output path leaked into seeds: {seeds:?}"
        );
        assert!(
            seeds.contains(&"/nix/store/cccccccccccccccccccccccccccccccc-src".to_string()),
            "input_source still present: {seeds:?}"
        );
    }
}
