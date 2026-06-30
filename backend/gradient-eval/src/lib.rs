/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Standalone Nix flake evaluator extracted from the gradient worker so both the
//! worker and the CLI (`gradient eval`) can drive the same evaluator.

pub mod eval_worker;
pub mod flake_walk;
pub mod jobs;
pub mod nix_eval;
pub mod stats;
pub mod wildcard_walk;

/// Returns the full `/nix/store/` path for a hash-name stored without prefix.
pub fn nix_store_path(hash_name: &str) -> String {
    if hash_name.starts_with('/') {
        hash_name.to_string()
    } else {
        format!("/nix/store/{hash_name}")
    }
}

/// Strips the `/nix/store/` prefix from a path, returning just the hash-name.
pub fn strip_nix_store_prefix(path: &str) -> String {
    path.strip_prefix("/nix/store/").unwrap_or(path).to_string()
}
