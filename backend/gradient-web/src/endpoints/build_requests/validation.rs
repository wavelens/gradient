/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Shared input validators for the build-request endpoints: manifest path
//! safety (no traversal / absolute / null bytes) and BLAKE3 hex parsing.

use crate::error::{WebError, WebResult};

pub fn validate_manifest_path(path: &str) -> WebResult<()> {
    if path.is_empty() {
        return Err(WebError::bad_request("Empty path in manifest"));
    }
    if path.contains('\0') {
        return Err(WebError::bad_request(format!(
            "Invalid path (null byte): {}",
            path
        )));
    }
    if path.starts_with('/') {
        return Err(WebError::bad_request(format!("Absolute path: {}", path)));
    }
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(WebError::bad_request(format!(
                "Invalid path component in: {}",
                path
            )));
        }
    }
    Ok(())
}

pub fn decode_blake3_hex(hash: &str) -> WebResult<Vec<u8>> {
    if hash.len() != 64
        || !hash
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    {
        return Err(WebError::bad_request(format!(
            "Hash must be 64-char lowercase hex: {}",
            hash
        )));
    }
    hex::decode(hash).map_err(|e| WebError::bad_request(format!("Invalid hash hex: {}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_traversal() {
        assert!(validate_manifest_path("../escape").is_err());
        assert!(validate_manifest_path("foo/../bar").is_err());
        assert!(validate_manifest_path("a/./b").is_err());
    }

    #[test]
    fn rejects_absolute_and_empty_paths() {
        assert!(validate_manifest_path("/absolute").is_err());
        assert!(validate_manifest_path("").is_err());
        assert!(validate_manifest_path(".").is_err());
    }

    #[test]
    fn rejects_null_bytes() {
        assert!(validate_manifest_path("with\0null").is_err());
    }

    #[test]
    fn accepts_normal_nested_paths() {
        assert!(validate_manifest_path("flake.nix").is_ok());
        assert!(validate_manifest_path("src/main.rs").is_ok());
        assert!(validate_manifest_path("a/b/c/d.txt").is_ok());
    }

    #[test]
    fn hash_must_be_64_lowercase_hex() {
        let ok = "a".repeat(64);
        assert!(decode_blake3_hex(&ok).is_ok());
        let too_short = "a".repeat(63);
        assert!(decode_blake3_hex(&too_short).is_err());
        let uppercase = "A".repeat(64);
        assert!(decode_blake3_hex(&uppercase).is_err());
        let non_hex = "g".repeat(64);
        assert!(decode_blake3_hex(&non_hex).is_err());
    }
}
