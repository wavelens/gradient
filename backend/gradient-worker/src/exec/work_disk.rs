/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::path::Path;

use tokio::process::Command;

const MKFS: &str = "mkfs.ext4";
const OUT_OF_SPACE: [&str; 3] = [
    "Could not allocate block",
    "Could not allocate inode",
    "No space left on device",
];

#[derive(Debug, thiserror::Error)]
pub enum WorkDiskError {
    #[error("the checkout does not fit in diskMb = {disk_mb}")]
    TooSmall { disk_mb: u64 },
    #[error("start {MKFS}: {0}")]
    Start(std::io::Error),
    #[error("{MKFS} failed: {0}")]
    Failed(String),
}

pub async fn build_work_disk(
    staging: &Path,
    image: &Path,
    disk_mb: u64,
) -> Result<(), WorkDiskError> {
    let output = Command::new(MKFS)
        .args(["-q", "-F", "-L", "work", "-d"])
        .arg(staging)
        .arg(image)
        .arg(format!("{disk_mb}M"))
        .output()
        .await
        .map_err(WorkDiskError::Start)?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    match OUT_OF_SPACE.iter().any(|message| stderr.contains(message)) {
        true => Err(WorkDiskError::TooSmall { disk_mb }),
        false => Err(WorkDiskError::Failed(stderr.trim().to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkout_of(bytes: usize, files: usize) -> tempfile::TempDir {
        let staging = tempfile::tempdir().unwrap();
        for n in 0..files {
            let content = vec![b'x'; bytes];
            std::fs::write(staging.path().join(format!("file-{n}.bin")), content).unwrap();
        }
        staging
    }

    #[tokio::test]
    async fn a_checkout_larger_than_the_disk_fails() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("work.img");
        let small = checkout_of(1_000_000, 3);
        build_work_disk(small.path(), &image, 64).await.unwrap();

        let large = checkout_of(10_000_000, 8);
        let error = build_work_disk(large.path(), &image, 64).await.unwrap_err();
        assert!(
            matches!(error, WorkDiskError::TooSmall { disk_mb: 64 }),
            "{error}"
        );
        assert_eq!(
            error.to_string(),
            "the checkout does not fit in diskMb = 64"
        );
    }

    #[tokio::test]
    async fn the_disk_holds_the_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("work.img");
        let checkout = checkout_of(4096, 2);
        std::fs::create_dir(checkout.path().join(".git")).unwrap();
        build_work_disk(checkout.path(), &image, 64).await.unwrap();

        let listing = Command::new("debugfs")
            .args(["-R", "ls -l /"])
            .arg(&image)
            .output()
            .await
            .unwrap();
        let listing = String::from_utf8_lossy(&listing.stdout);
        for name in ["file-0.bin", "file-1.bin", ".git"] {
            assert!(listing.contains(name), "{name} missing in {listing}");
        }
    }
}
