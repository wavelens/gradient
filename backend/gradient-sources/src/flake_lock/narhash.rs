/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use futures::StreamExt as _;
use gradient_wire::traits::InputProgressSink;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
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

fn extract_targz_stripped(archive: impl Read, dest: &Path) -> Result<()> {
    let decoder = flate2::read::GzDecoder::new(std::io::BufReader::new(archive));
    let mut archive = tar::Archive::new(decoder);
    archive.set_preserve_permissions(false);
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
        if !stripped
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        {
            bail!("tar entry {} outside the source directory", path.display());
        }
        if entry.header().entry_type().is_hard_link() {
            bail!("tar entry {} is a hard link", path.display());
        }
        check_existing_ancestor_directories_are_not_symlinks(dest, &stripped)?;

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

fn check_existing_ancestor_directories_are_not_symlinks(dest: &Path, entry: &Path) -> Result<()> {
    let mut ancestor = dest.to_path_buf();
    for component in entry.parent().into_iter().flat_map(Path::components) {
        ancestor.push(component);
        match std::fs::symlink_metadata(&ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => bail!(
                "tar entry {} went through symlink {}",
                entry.display(),
                ancestor.display()
            ),
            Ok(_) => {}
            Err(_) => break,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn targz(entries: &[(&[u8], tar::EntryType, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for &(name, kind, content) in entries {
            let mut header = tar::Header::new_old();
            header.as_old_mut().name[..name.len()].copy_from_slice(name);
            header.set_entry_type(kind);
            header.set_mode(0o644);
            if kind.is_symlink() {
                header.as_old_mut().linkname[..content.len()].copy_from_slice(content);
                header.set_size(0);
                header.set_cksum();
                builder.append(&header, std::io::empty()).unwrap();
            } else {
                header.set_size(content.len() as u64);
                header.set_cksum();
                builder.append(&header, content).unwrap();
            }
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn extract(entries: &[(&[u8], tar::EntryType, &[u8])]) -> (tempfile::TempDir, Result<()>) {
        let outer = tempfile::tempdir().unwrap();
        let dest = outer.path().join("source");
        std::fs::create_dir(&dest).unwrap();
        let outcome = extract_targz_stripped(&targz(entries)[..], &dest);
        (outer, outcome)
    }

    #[test]
    fn a_regular_tarball_with_a_symlink_is_extracted_without_its_top_directory() {
        let (outer, outcome) = extract(&[
            (b"repo-abc/README", tar::EntryType::Regular, b"hello"),
            (b"repo-abc/link", tar::EntryType::Symlink, b"README"),
        ]);
        outcome.unwrap();
        let dest = outer.path().join("source");
        assert_eq!(std::fs::read(dest.join("README")).unwrap(), b"hello");
        assert!(dest.join("link").symlink_metadata().unwrap().is_symlink());
    }

    #[test]
    fn an_entry_with_a_parent_component_is_rejected() {
        let (outer, outcome) = extract(&[(b"repo-abc/../escaped", tar::EntryType::Regular, b"x")]);
        assert!(outcome.is_err());
        assert!(!outer.path().join("escaped").exists());
    }

    #[test]
    fn an_entry_written_through_a_symlinked_directory_is_rejected() {
        let (outer, outcome) = extract(&[
            (b"repo-abc/out", tar::EntryType::Symlink, b".."),
            (b"repo-abc/out/escaped", tar::EntryType::Regular, b"x"),
        ]);
        assert!(outcome.is_err());
        assert!(!outer.path().join("escaped").exists());
    }

    #[test]
    fn a_hard_link_entry_is_rejected() {
        let (_outer, outcome) = extract(&[(b"repo-abc/link", tar::EntryType::Link, b"")]);
        assert!(outcome.is_err());
    }
}
