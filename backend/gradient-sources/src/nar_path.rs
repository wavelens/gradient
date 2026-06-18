/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::SourceError;
use gradient_types::*;
use anyhow::Result;

pub fn get_hash_from_url(url: String) -> Result<String, SourceError> {
    let path_split = url.split('.').collect::<Vec<&str>>();

    // Check if we have exactly 2 or 3 parts (hash.extension[.compression])
    if !(path_split.len() == 2 || path_split.len() == 3) {
        return Err(SourceError::InvalidPath);
    }

    // Accept 32-char store-path hashes (160-bit nix32) and 52-char file/nar hashes (256-bit nix32)
    if path_split[0].len() != 32 && path_split[0].len() != 52 {
        return Err(SourceError::InvalidPath);
    }

    // Check extension
    if !((path_split[1] == "narinfo" && path_split.len() == 2) || path_split[1] == "nar") {
        return Err(SourceError::InvalidPath);
    }

    // Check hash characters (base32) - exclude 'e', 'o', 't', 'u'
    if !path_split[0]
        .chars()
        .all(|c| "0123456789abcdfghijklmnpqrsvwxyz".contains(c))
    {
        return Err(SourceError::InvalidPath);
    }

    Ok(path_split[0].to_string())
}

pub fn get_hash_from_path(path: String) -> Result<(String, String), SourceError> {
    let path_split = path.split('/').collect::<Vec<&str>>();
    if path_split.len() < 4 {
        return Err(SourceError::InvalidPath);
    }

    let path_split = path_split[3].split('-').collect::<Vec<&str>>();
    if path_split.len() < 2 {
        return Err(SourceError::InvalidPath);
    }

    let package = path_split[1..].join("-");
    let hash = path_split[0].to_string();

    Ok((hash, package))
}

pub fn get_path_from_derivation_output(output: MDerivationOutput) -> StorePath {
    StorePath::from_parts(output.hash, output.package)
}

/// Parses a bare `<hash>-<name>.drv` (no `/nix/store/` prefix) into its hash
/// and name components. `name` includes everything after the first `-` minus
/// the trailing `.drv` suffix. Returns `InvalidPath` when the input is not in
/// `<hash>-<name>.drv` form.
pub fn parse_drv_hash_name(drv_path: &str) -> Result<(String, String), SourceError> {
    let (hash, rest) = drv_path.split_once('-').ok_or(SourceError::InvalidPath)?;
    let name = rest.strip_suffix(".drv").ok_or(SourceError::InvalidPath)?;
    if hash.is_empty() || name.is_empty() {
        return Err(SourceError::InvalidPath);
    }
    Ok((hash.to_string(), name.to_string()))
}

pub fn get_cache_nar_location(base_path: String, hash: String) -> Result<String, SourceError> {
    let hash_hex = hash.as_str();
    std::fs::create_dir_all(format!("{}/nars/{}", base_path, &hash_hex[0..2])).map_err(|e| {
        SourceError::FileRead {
            reason: e.to_string(),
        }
    })?;

    Ok(format!(
        "{}/nars/{}/{}.nar",
        base_path,
        &hash_hex[0..2],
        &hash_hex[2..],
    ))
}

/// Returns the on-disk path for a compressed (zstd) NAR cache file.
/// Used for non-entry-point builds that are cached on first serve.
pub fn get_cache_nar_compressed_location(
    base_path: String,
    hash: String,
) -> Result<String, SourceError> {
    let hash_hex = hash.as_str();
    std::fs::create_dir_all(format!("{}/nars/{}", base_path, &hash_hex[0..2])).map_err(|e| {
        SourceError::FileRead {
            reason: e.to_string(),
        }
    })?;

    Ok(format!(
        "{}/nars/{}/{}.nar.zst",
        base_path,
        &hash_hex[0..2],
        &hash_hex[2..],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_drv_form() {
        let (h, n) =
            parse_drv_hash_name("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello-2.12.1.drv").unwrap();
        assert_eq!(h, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(n, "hello-2.12.1");
    }

    #[test]
    fn rejects_missing_drv_suffix() {
        assert!(parse_drv_hash_name("abc-foo").is_err());
    }

    #[test]
    fn rejects_missing_hash_or_name() {
        assert!(parse_drv_hash_name("-foo.drv").is_err());
        assert!(parse_drv_hash_name("abc-.drv").is_err());
    }

    #[test]
    fn rejects_missing_dash() {
        assert!(parse_drv_hash_name("abcfoo.drv").is_err());
    }
}
