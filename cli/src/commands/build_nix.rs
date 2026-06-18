/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `nix`-feature fast paths for `gradient build`: pack the source NAR locally
//! (skipping the per-file blob manifest) and substitute the built output back
//! into the local store as a `result` symlink (instead of downloading products).

use crate::commands::build::{TrackedFile, select_primary_entry_point};
use crate::input::server_base;
use crate::output::{ExitKind, Output};
use connector::build_requests::DispatchResponse;
use connector::evals::ArtefactTree;
use futures::StreamExt as _;
use harmonia_file_nar::NarByteStream;

/// Stage the git-tracked files into a temp dir, NAR-pack them with the same
/// serialiser the server uses (so the store path matches), and upload the NAR.
pub async fn dispatch_via_nar(
    client: &connector::Client,
    organization: &str,
    target: Option<String>,
    system: Option<String>,
    entries: &[TrackedFile],
    quiet: bool,
    out: Output,
) -> DispatchResponse {
    let staging = tempfile::tempdir()
        .unwrap_or_else(|e| out.err(ExitKind::Api, format!("Failed to create temp dir: {}", e)));

    for entry in entries {
        let dest = staging.path().join(&entry.path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).unwrap_or_else(|e| {
                out.err(ExitKind::Api, format!("Failed to stage {}: {}", entry.path, e))
            });
        }
        std::fs::copy(&entry.abs, &dest).unwrap_or_else(|e| {
            out.err(ExitKind::Api, format!("Failed to stage {}: {}", entry.path, e))
        });
    }

    let mut stream = NarByteStream::new(staging.path().to_path_buf());
    let mut nar = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.unwrap_or_else(|e| out.err(ExitKind::Api, format!("Failed to pack NAR: {}", e)));
        nar.extend_from_slice(&chunk);
    }

    if !quiet {
        out.human(format!(
            "Packed source NAR ({} bytes) from {} tracked files, uploading...",
            nar.len(),
            entries.len()
        ));
    }

    match client
        .build_requests()
        .upload_source_nar(organization, target.as_deref(), system.as_deref(), nar)
        .await
    {
        Ok(d) => d,
        Err(e) => out.err(ExitKind::Api, format!("Failed to upload source NAR: {}", e)),
    }
}

/// Substitute every built output from the gradient cache and create a single
/// GC-rooted `result` symlink to the primary output.
pub async fn link_result(
    dispatch: &DispatchResponse,
    tree: &ArtefactTree,
    target: Option<&str>,
    out: Output,
) {
    let Some(cache) = dispatch.cache.as_deref() else {
        out.human("Organization has no cache; skipping output substitution.");
        return;
    };

    let base = server_base(out);
    let cache_url = format!("{}/cache/{}", base.trim_end_matches('/'), cache);

    for ep in &tree.entry_points {
        for output in &ep.outputs {
            let store_path = output.full_store_path();
            let status = tokio::process::Command::new("nix")
                .args(["copy", "--from", &cache_url, "--no-check-sigs", &store_path])
                .status()
                .await;
            if !matches!(status, Ok(s) if s.success()) {
                out.progress(format!("warning: nix copy failed for {store_path}"));
            }
        }
    }

    let Some(primary) = select_primary_entry_point(tree, target) else {
        out.human("No outputs to link.");
        return;
    };

    let Some(out_path) = primary
        .outputs
        .iter()
        .find(|o| o.name == "out")
        .or_else(|| primary.outputs.first())
    else {
        return;
    };

    let realise_path = out_path.full_store_path();
    let status = tokio::process::Command::new("nix-store")
        .args(["--realise", &realise_path, "--add-root", "result", "--indirect"])
        .status()
        .await;
    match status {
        Ok(s) if s.success() => out.human(format!("result -> {realise_path}")),
        _ => out.progress("warning: could not create result symlink"),
    }
}
