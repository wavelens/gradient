/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use tokio::process::Command;

pub const WRITABLE_STORE_IMAGE: &str = "writable-store.img";
pub const WRITABLE_STORE_LABEL: &str = "writable-store";
const MKFS: &str = "mkfs.ext4";

pub async fn create_writable_store(guest_dir: &Path, size_mb: u64) -> Result<PathBuf> {
    let image = guest_dir.join(WRITABLE_STORE_IMAGE);
    let output = Command::new(MKFS)
        .args(["-q", "-F", "-L", WRITABLE_STORE_LABEL])
        .args(["-E", "lazy_itable_init=1,lazy_journal_init=1"])
        .arg(&image)
        .arg(format!("{size_mb}M"))
        .output()
        .await
        .with_context(|| format!("start {MKFS}"))?;
    ensure!(
        output.status.success(),
        "{MKFS} {} failed: {}",
        image.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(image)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;

    use super::*;

    #[tokio::test]
    async fn the_writable_store_is_a_sparse_labeled_ext4_image() {
        let dir = tempfile::tempdir().unwrap();
        let image = create_writable_store(dir.path(), 4096).await.unwrap();

        let metadata = std::fs::metadata(&image).unwrap();
        assert_eq!(metadata.len(), 4096 * 1024 * 1024);
        let allocated = metadata.blocks() * 512;
        assert!(
            allocated < metadata.len() / 20,
            "{allocated} bytes allocated"
        );
        let header = Command::new("dumpe2fs")
            .arg("-h")
            .arg(&image)
            .output()
            .await
            .unwrap();
        let header = String::from_utf8_lossy(&header.stdout);
        let label = header
            .lines()
            .find_map(|line| line.strip_prefix("Filesystem volume name:"))
            .map(str::trim);
        assert_eq!(label, Some(WRITABLE_STORE_LABEL), "{header}");
    }
}
