/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Pure-Rust reader for nix's per-derivation build logs
//! (`$NIX_LOG_DIR/drvs/<first2>/<rest>.bz2`), used when a derivation is already
//! built in the local store so the daemon produces no fresh log. Avoids
//! shelling out to `nix log`.

use anyhow::Result;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Compute the bzip2 log path nix uses for a derivation store path.
pub fn store_log_path(log_dir: &Path, drv_path: &str) -> PathBuf {
    let base = drv_path.rsplit('/').next().unwrap_or(drv_path);
    let split = 2.min(base.len());
    let (first2, rest) = base.split_at(split);
    log_dir.join("drvs").join(first2).join(format!("{rest}.bz2"))
}

/// Read and decompress the nix-store build log for `drv_path`, if present.
/// Tries the bzip2 file first, then the uncompressed sibling. Returns `None`
/// when no log exists.
pub fn read_store_build_log(drv_path: &str) -> Result<Option<String>> {
    let log_dir = std::env::var("NIX_LOG_DIR").unwrap_or_else(|_| "/nix/var/log/nix".into());
    let bz2 = store_log_path(Path::new(&log_dir), drv_path);
    if bz2.exists() {
        let data = std::fs::read(&bz2)?;
        let mut out = String::new();
        bzip2::read::BzDecoder::new(&data[..]).read_to_string(&mut out)?;
        return Ok(Some(out));
    }
    let plain = bz2.with_extension("");
    if plain.exists() {
        return Ok(Some(std::fs::read_to_string(&plain)?));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::store_log_path;
    use std::path::Path;

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
