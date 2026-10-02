/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::io::Read as _;

use anyhow::{Context, Result};
use harmonia_utils_hash::fmt::Any;
use harmonia_utils_hash::{Hash, HashView as _};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    None,
    Zstd,
    Xz,
    Bzip2,
}

pub fn detect_compression(url: &str) -> Compression {
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

pub const NAR_MAGIC: &[u8] = b"\x0d\x00\x00\x00\x00\x00\x00\x00nix-archive-1";

pub const SNIFF_BYTES: usize = NAR_MAGIC.len();

pub fn sniff_compression(bytes: &[u8]) -> Option<Compression> {
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

/// The bytes are winning over the URL because caches can disagree with their own metadata. Attic is
/// serving `Compression: zstd` under a `nar/<hash>.nar` URL, and the daemon would reject the zstd
/// frame as a raw NAR.
pub fn resolve_compression(bytes: &[u8], url: Option<&str>) -> Compression {
    sniff_compression(bytes)
        .unwrap_or_else(|| url.map(detect_compression).unwrap_or(Compression::Zstd))
}

pub fn decompress(compressed: &[u8], kind: Compression) -> Result<Vec<u8>> {
    decompress_reader(std::io::Cursor::new(compressed), kind)
}

pub fn decompress_reader<R: std::io::Read>(reader: R, kind: Compression) -> Result<Vec<u8>> {
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

pub async fn extract_single_file_from_nar(nar_bytes: &[u8]) -> Result<Vec<u8>> {
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

pub fn parse_nar_hash_to_bytes(s: &str) -> Result<[u8; 32]> {
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
        assert_eq!(
            detect_compression("https://s3.example/abc.nar.xz?sig=XYZ&exp=1"),
            Compression::Xz
        );
        assert_eq!(
            detect_compression("https://example/some/opaque"),
            Compression::Zstd
        );
    }

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

    #[test]
    fn sniff_falls_back_to_the_url_when_no_magic_matches() {
        assert_eq!(sniff_compression(b"not a known container"), None);
        assert_eq!(sniff_compression(b""), None);
        assert_eq!(
            resolve_compression(b"opaque", Some("https://cache.example/nar/abc.nar.xz")),
            Compression::Xz
        );
        assert_eq!(resolve_compression(b"opaque", None), Compression::Zstd);
    }

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
        let nix32 = "sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73";
        let bytes = parse_nar_hash_to_bytes(nix32).unwrap();
        let expected: [u8; 32] = Sha256::digest(b"").into();
        assert_eq!(bytes, expected);
    }
}
