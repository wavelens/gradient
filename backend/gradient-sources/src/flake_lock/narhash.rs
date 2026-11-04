/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use base64::Engine as _;
use futures::StreamExt as _;
use gradient_wire::traits::InputProgressSink;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt as _;

pub async fn nar_hash_of_dir(path: &Path) -> Result<String> {
    let mut stream = harmonia_file_nar::NarByteStream::new(path.to_path_buf());
    let mut hasher = Sha256::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("read NAR byte stream")?;
        hasher.update(&chunk);
    }

    let digest = hasher.finalize();

    Ok(format!(
        "sha256-{}",
        base64::engine::general_purpose::STANDARD.encode(digest)
    ))
}

pub async fn tarball_source_nar_hash(
    req: reqwest::RequestBuilder,
    progress: &dyn InputProgressSink,
) -> Result<String> {
    let resp = req
        .send()
        .await
        .context("fetch source tarball")?
        .error_for_status()
        .context("request the source tarball")?;
    let archive = download_to_temp_file(resp, progress).await?;

    let tmp = tempfile::tempdir().context("create extraction temp dir")?;
    let dest = tmp.path().to_path_buf();
    tokio::task::spawn_blocking(move || extract_targz_stripped(archive.as_file(), &dest))
        .await
        .context("extraction task panicked")??;

    nar_hash_of_dir(tmp.path()).await
}

async fn download_to_temp_file(
    mut resp: reqwest::Response,
    progress: &dyn InputProgressSink,
) -> Result<tempfile::NamedTempFile> {
    let archive = tempfile::NamedTempFile::new().context("Create a source tarball file")?;
    let mut file =
        tokio::fs::File::from_std(archive.reopen().context("Create a source tarball file")?);
    let total = resp.content_length();
    let mut done = 0;
    while let Some(chunk) = resp.chunk().await.context("read source tarball body")? {
        file.write_all(&chunk)
            .await
            .context("Write the source tarball.")?;
        done += chunk.len() as u64;
        progress.transfer(done, total);
    }
    file.flush().await.context("Write the source tarball.")?;

    Ok(archive)
}

fn extract_targz_stripped(archive: &std::fs::File, dest: &Path) -> Result<()> {
    let decoder = flate2::read::GzDecoder::new(std::io::BufReader::new(archive));
    let mut archive = tar::Archive::new(decoder);
    archive.set_preserve_permissions(true);
    archive.set_preserve_mtime(false);

    for entry in archive.entries().context("read tar entries")? {
        let mut entry = entry.context("read tar entry")?;
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
            .with_context(|| format!("unpack {}", out.display()))?;
    }

    Ok(())
}
