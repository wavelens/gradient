/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::types::*;

/// Returns the full `/nix/store/` path for a derivation hash-name stored without prefix.
pub fn nix_store_path(hash_name: &str) -> String {
    if hash_name.starts_with('/') {
        hash_name.to_string()
    } else {
        format!("/nix/store/{}", hash_name)
    }
}

/// Strips the `/nix/store/` prefix from a path, returning just the hash-name component.
pub fn strip_nix_store_prefix(path: &str) -> String {
    path.strip_prefix("/nix/store/").unwrap_or(path).to_string()
}

/// Strips the `/nix/store/` prefix, returning a `&str` (no allocation).
pub fn strip_store_prefix(path: &str) -> &str {
    path.strip_prefix("/nix/store/").unwrap_or(path)
}

pub fn get_derivation_paths(derivations: &[MDerivation]) -> Vec<String> {
    derivations
        .iter()
        .map(|d| nix_store_path(&d.derivation_path))
        .collect()
}
