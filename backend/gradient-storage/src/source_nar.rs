/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Pure-Rust materialisation of a `/nix/store/<hash>-source` path from a staging directory.
//!
//! Algorithm:
//!  1. Walk the directory and serialise its contents as a canonical NAR via `NarByteStream`.
//!  2. SHA-256 the NAR bytes → `nar_hash`.
//!  3. Build a `NixArchive` content address and call `make_store_path_from_ca` with name
//!     "source", producing the same path `nix-store --add` would assign.

use anyhow::{Context, Result};
use futures::StreamExt as _;
use harmonia_file_nar::NarByteStream;
use harmonia_store_content_address::{ContentAddress, make_store_path_from_ca};
use harmonia_store_path::{StoreDir, StorePathName};
use harmonia_utils_hash::{Algorithm, Hash, HashFormat as _, Sha256};
use std::path::Path;

pub struct SourceNar {
    pub store_path: String,
    pub store_hash: String,
    pub nar_bytes: Vec<u8>,
    pub nar_size: u64,
    pub nar_hash_sri: String,
    pub nar_hash_nix32: String,
    /// zstd-compressed NAR as persisted in `NarStore` (which stores `.nar.zst`).
    pub compressed_bytes: Vec<u8>,
    /// Size and SHA-256 of `compressed_bytes`, i.e. the narinfo `FileSize`/`FileHash`.
    pub file_size: u64,
    pub file_hash_sri: String,
}

async fn nar_bytes_from_dir(staging_dir: &Path) -> Result<Vec<u8>> {
    let mut stream = NarByteStream::new(staging_dir.to_path_buf());
    let mut nar_bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("NAR serialisation error")?;
        nar_bytes.extend_from_slice(&chunk);
    }

    Ok(nar_bytes)
}

pub async fn materialise_source_nar(staging_dir: &Path) -> Result<SourceNar> {
    source_nar_from_bytes(nar_bytes_from_dir(staging_dir).await?).await
}

/// Compute the `/nix/store/<hash>-source` path and metadata from a NAR packed
/// elsewhere (e.g. the `nix`-feature CLI), keeping the server authoritative on
/// the store path.
pub async fn source_nar_from_bytes(nar_bytes: Vec<u8>) -> Result<SourceNar> {
    let nar_size = nar_bytes.len() as u64;
    let raw_hash = Sha256::digest(&nar_bytes);

    let nix32_str = format!("{:#}", raw_hash.as_base32());
    let nar_hash_nix32 = format!("sha256:{nix32_str}");
    let nar_hash_sri = raw_hash.as_sri().to_string();

    let store_dir = StoreDir::default();
    let name: StorePathName = "source"
        .parse()
        .expect("'source' is a valid store path name");
    let hash_for_ca = Hash::new(Algorithm::SHA256, raw_hash.digest_bytes());
    let ca = ContentAddress::NixArchive(hash_for_ca);
    let store_path_obj = make_store_path_from_ca(&store_dir, name, ca);

    let store_hash = store_path_obj.hash().to_string();
    let store_path = gradient_types::StorePath::from_parts(store_hash.clone(), "source").full();

    let compressed_bytes = zstd::encode_all(
        std::io::Cursor::new(&nar_bytes),
        gradient_types::constants::NAR_ZSTD_LEVEL,
    )
    .context("failed to zstd-compress source NAR")?;
    let file_size = compressed_bytes.len() as u64;
    let file_hash_sri = Sha256::digest(&compressed_bytes).as_sri().to_string();

    Ok(SourceNar {
        store_path,
        store_hash,
        nar_bytes,
        nar_size,
        nar_hash_sri,
        nar_hash_nix32,
        compressed_bytes,
        file_size,
        file_hash_sri,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_staging_dir() -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        fs::write(dir.path().join("hello.txt"), b"hello gradient\n").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/world.txt"), b"world\n").unwrap();
        dir
    }

    #[tokio::test]
    async fn deterministic_store_path() {
        let dir = make_staging_dir();
        let a = materialise_source_nar(dir.path())
            .await
            .expect("first call");
        let b = materialise_source_nar(dir.path())
            .await
            .expect("second call");
        assert_eq!(a.store_path, b.store_path);
        assert_eq!(a.nar_hash_nix32, b.nar_hash_nix32);
    }

    #[tokio::test]
    async fn from_bytes_matches_dir() {
        let dir = make_staging_dir();
        let from_dir = materialise_source_nar(dir.path()).await.unwrap();
        let from_bytes = source_nar_from_bytes(from_dir.nar_bytes.clone())
            .await
            .unwrap();
        assert_eq!(from_dir.store_path, from_bytes.store_path);
        assert_eq!(from_dir.store_hash, from_bytes.store_hash);
        assert_eq!(from_dir.nar_hash_nix32, from_bytes.nar_hash_nix32);
        assert_eq!(from_dir.nar_hash_sri, from_bytes.nar_hash_sri);
        assert_eq!(from_dir.nar_size, from_bytes.nar_size);
    }

    #[tokio::test]
    async fn compressed_bytes_round_trip_to_nar() {
        let dir = make_staging_dir();
        let nar = materialise_source_nar(dir.path()).await.unwrap();

        assert_ne!(
            nar.compressed_bytes, nar.nar_bytes,
            "stored bytes must be compressed, not the raw NAR"
        );
        let decompressed = zstd::decode_all(std::io::Cursor::new(&nar.compressed_bytes)).unwrap();
        assert_eq!(decompressed, nar.nar_bytes);

        assert_eq!(nar.file_size, nar.compressed_bytes.len() as u64);
        let expected_file_hash = Sha256::digest(&nar.compressed_bytes).as_sri().to_string();
        assert_eq!(nar.file_hash_sri, expected_file_hash);
    }

    #[tokio::test]
    async fn store_path_ends_in_source() {
        let dir = make_staging_dir();
        let result = materialise_source_nar(dir.path())
            .await
            .expect("materialise");
        assert!(
            result.store_path.ends_with("-source"),
            "store path should end with '-source', got: {}",
            result.store_path
        );
    }

    #[tokio::test]
    async fn store_path_shape() {
        let dir = make_staging_dir();
        let result = materialise_source_nar(dir.path())
            .await
            .expect("materialise");
        assert!(result.store_path.starts_with("/nix/store/"));
        let base = result.store_path.strip_prefix("/nix/store/").unwrap();
        let (hash_part, name_part) = base.split_once('-').expect("store path has dash");
        assert_eq!(hash_part.len(), 32, "hash portion must be 32 chars");
        assert_eq!(name_part, "source");
        assert_eq!(result.store_hash.len(), 32);
    }
}
