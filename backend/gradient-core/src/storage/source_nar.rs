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
use harmonia_utils_hash::fmt::CommonHash as _;
use harmonia_utils_hash::{Algorithm, Hash, Sha256};
use std::path::Path;

pub struct SourceNar {
    pub store_path: String,
    pub store_hash: String,
    pub nar_bytes: Vec<u8>,
    pub nar_size: u64,
    pub nar_hash_sri: String,
    pub nar_hash_nix32: String,
}

pub async fn materialise_source_nar(staging_dir: &Path) -> Result<SourceNar> {
    let mut stream = NarByteStream::new(staging_dir.to_path_buf());
    let mut nar_bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("NAR serialisation error")?;
        nar_bytes.extend_from_slice(&chunk);
    }

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
    let store_path = format!("/nix/store/{store_hash}-source");

    Ok(SourceNar {
        store_path,
        store_hash,
        nar_bytes,
        nar_size,
        nar_hash_sri,
        nar_hash_nix32,
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
