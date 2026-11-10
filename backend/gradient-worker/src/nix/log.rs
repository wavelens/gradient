/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The daemon is producing no fresh log for a derivation already built in the local
//! store. This reader is reading nix's stored log instead of shelling out to `nix log`.

use anyhow::Result;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

pub fn store_log_path(log_dir: &Path, drv_path: &str) -> PathBuf {
    let base = drv_path.rsplit('/').next().unwrap_or(drv_path);
    let split = 2.min(base.len());
    let (first2, rest) = base.split_at(split);
    log_dir
        .join("drvs")
        .join(first2)
        .join(format!("{rest}.bz2"))
}

#[expect(
    clippy::disallowed_methods,
    reason = "NIX_LOG_DIR is a variable of Nix, not an option of Gradient"
)]
pub fn nix_log_dir() -> PathBuf {
    std::env::var_os("NIX_LOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/nix/var/log/nix"))
}

#[derive(Debug, PartialEq, Eq)]
pub struct CappedNixBuildLog {
    pub text: Vec<u8>,
    pub truncated: bool,
}

pub fn read_store_build_log(
    log_dir: &Path,
    drv_path: &str,
    max_bytes: u64,
) -> Result<Option<CappedNixBuildLog>> {
    let bz2 = store_log_path(log_dir, drv_path);
    if bz2.exists() {
        let file = BufReader::new(File::open(&bz2)?);
        return read_capped(bzip2::read::BzDecoder::new(file), max_bytes).map(Some);
    }
    let plain = bz2.with_extension("");
    if plain.exists() {
        return read_capped(File::open(&plain)?, max_bytes).map(Some);
    }
    Ok(None)
}

fn read_capped(reader: impl Read, max_bytes: u64) -> Result<CappedNixBuildLog> {
    let mut text = Vec::new();
    reader
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut text)?;
    let truncated = text.len() as u64 > max_bytes;
    text.truncate(usize::try_from(max_bytes).unwrap_or(usize::MAX));
    Ok(CappedNixBuildLog { text, truncated })
}

#[cfg(test)]
mod tests {
    use super::{CappedNixBuildLog, read_store_build_log, store_log_path};
    use std::io::Write;
    use std::path::Path;

    const DRV: &str = "/nix/store/abcd1234efgh5678-foo.drv";

    fn log_file(dir: &Path, name: &str) -> std::path::PathBuf {
        let path = dir.join("drvs/ab").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        path
    }

    #[test]
    fn reads_the_bzip2_log() {
        let dir = tempfile::tempdir().unwrap();
        let mut enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        enc.write_all(b"building foo\n").unwrap();
        std::fs::write(
            log_file(dir.path(), "cd1234efgh5678-foo.drv.bz2"),
            enc.finish().unwrap(),
        )
        .unwrap();

        assert_eq!(
            read_store_build_log(dir.path(), DRV, 1024).unwrap(),
            Some(CappedNixBuildLog {
                text: b"building foo\n".to_vec(),
                truncated: false
            })
        );
    }

    #[test]
    fn a_log_longer_than_the_limit_is_cut() {
        let dir = tempfile::tempdir().unwrap();
        let mut enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        enc.write_all(&[b'x'; 4096]).unwrap();
        std::fs::write(
            log_file(dir.path(), "cd1234efgh5678-foo.drv.bz2"),
            enc.finish().unwrap(),
        )
        .unwrap();

        assert_eq!(
            read_store_build_log(dir.path(), DRV, 10).unwrap(),
            Some(CappedNixBuildLog {
                text: b"xxxxxxxxxx".to_vec(),
                truncated: true
            })
        );
    }

    #[test]
    fn falls_back_to_the_plain_log() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(log_file(dir.path(), "cd1234efgh5678-foo.drv"), "plain\n").unwrap();

        assert_eq!(
            read_store_build_log(dir.path(), DRV, 6).unwrap(),
            Some(CappedNixBuildLog {
                text: b"plain\n".to_vec(),
                truncated: false
            })
        );
    }

    #[test]
    fn no_log_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_store_build_log(dir.path(), DRV, 1024).unwrap(), None);
    }

    #[test]
    fn computes_drvs_path() {
        let p = store_log_path(
            Path::new("/nix/var/log/nix"),
            "/nix/store/abcd1234efgh5678-foo.drv",
        );
        assert_eq!(
            p,
            Path::new("/nix/var/log/nix/drvs/ab/cd1234efgh5678-foo.drv.bz2")
        );
    }

    #[test]
    fn handles_bare_basename() {
        let p = store_log_path(Path::new("/log"), "xyfoo-bar.drv");
        assert_eq!(p, Path::new("/log/drvs/xy/foo-bar.drv.bz2"));
    }
}
