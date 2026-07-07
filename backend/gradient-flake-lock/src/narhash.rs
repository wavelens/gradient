/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Native `narHash` recomputation.
//!
//! Mirrors the fetcher nix uses for each node: github/gitlab fetch the codeload
//! tarball and NAR-serialize the unpacked tree (top-level prefix stripped); git
//! NAR-serializes the checked-out worktree minus `.git`. The NAR bytes come from
//! `harmonia-file-nar`, sha256'd into an SRI `sha256-<base64>` string.

use anyhow::{Context, Result};
use base64::Engine as _;
use futures::StreamExt as _;
use sha2::{Digest, Sha256};
use std::io::Cursor;
use std::path::{Path, PathBuf};

/// NAR-serialize `path` and return its hash as an SRI `sha256-<base64>` string,
/// the format nix writes into a `flake.lock` `narHash`.
pub async fn nar_hash_of_dir(path: &Path) -> Result<String> {
    let mut stream = harmonia_file_nar::NarByteStream::new(path.to_path_buf());
    let mut hasher = Sha256::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("reading NAR byte stream")?;
        hasher.update(&chunk);
    }

    let digest = hasher.finalize();

    Ok(format!(
        "sha256-{}",
        base64::engine::general_purpose::STANDARD.encode(digest)
    ))
}

/// Fetch a `.tar.gz` source archive via `req`, unpack it with its single
/// top-level directory stripped, and return the NAR hash of the tree.
pub async fn tarball_source_nar_hash(req: reqwest::RequestBuilder) -> Result<String> {
    let resp = req
        .send()
        .await
        .context("fetching source tarball")?
        .error_for_status()
        .context("source tarball request failed")?;
    let bytes = resp
        .bytes()
        .await
        .context("reading source tarball body")?
        .to_vec();

    let tmp = tempfile::tempdir().context("creating extraction temp dir")?;
    let dest = tmp.path().to_path_buf();
    tokio::task::spawn_blocking(move || extract_targz_stripped(&bytes, &dest))
        .await
        .context("extraction task panicked")??;

    nar_hash_of_dir(tmp.path()).await
}

/// Unpack a gzipped tar archive into `dest`, stripping the single leading path
/// component every entry shares (`<owner>-<repo>-<sha>/`), preserving modes,
/// symlinks, and executable bits so the NAR matches nix's unpacked tree.
fn extract_targz_stripped(bytes: &[u8], dest: &Path) -> Result<()> {
    let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    archive.set_preserve_permissions(true);
    archive.set_preserve_mtime(false);

    for entry in archive.entries().context("reading tar entries")? {
        let mut entry = entry.context("reading tar entry")?;
        let path = entry.path().context("entry path")?.into_owned();

        let mut comps = path.components();
        comps.next();
        let stripped: PathBuf = comps.as_path().to_path_buf();
        if stripped.as_os_str().is_empty() {
            continue;
        }

        let out = dest.join(&stripped);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("mkdir {}", parent.display()))?;
        }

        entry
            .unpack(&out)
            .with_context(|| format!("unpacking {}", out.display()))?;
    }

    Ok(())
}
