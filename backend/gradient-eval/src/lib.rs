/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod eval_worker;
pub mod flake_walk;
mod frames;
pub mod ipc;
pub mod jobs;
pub mod nix_eval;
pub mod stats;
pub mod wildcard_walk;

pub fn nix_store_path(hash_name: &str) -> String {
    if hash_name.starts_with('/') {
        hash_name.to_string()
    } else {
        format!("/nix/store/{hash_name}")
    }
}

pub fn strip_nix_store_prefix(path: &str) -> String {
    path.strip_prefix("/nix/store/").unwrap_or(path).to_string()
}
