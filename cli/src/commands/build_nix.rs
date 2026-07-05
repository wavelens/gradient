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
use crate::output::{ExitKind, Output, to_exit_kind};
use connector::ConnectorError;
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

    let nar_len = nar.len();
    let file_count = entries.len();

    if !quiet {
        out.human(format!(
            "Packed source NAR ({nar_len} bytes) from {file_count} tracked files, uploading..."
        ));
    }

    match client
        .build_requests()
        .upload_source_nar(organization, target.as_deref(), system.as_deref(), nar)
        .await
    {
        Ok(d) => d,
        Err(e) => out.err(
            to_exit_kind(&e),
            upload_error_message(&e, nar_len, file_count, organization),
        ),
    }
}

fn human_bytes(n: usize) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} bytes")
    } else {
        format!("{value:.1} {} ({n} bytes)", UNITS[unit])
    }
}

/// Turn a source-NAR upload failure into an actionable message: always name the
/// size and file count that was attempted, and give status-specific guidance for
/// the common 413 (too large) and 400 (unparseable body) cases instead of dumping
/// the raw reverse-proxy error page.
fn upload_error_message(
    err: &ConnectorError,
    nar_len: usize,
    file_count: usize,
    organization: &str,
) -> String {
    let what = format!(
        "source NAR {} from {file_count} files to organization '{organization}'",
        human_bytes(nar_len)
    );
    match err {
        ConnectorError::Api { status, .. } if status.as_u16() == 413 => format!(
            "Failed to upload {what}: the server rejected it as too large (HTTP 413). \
             Raise the source upload limit on the server (GRADIENT_MAX_SOURCE_UPLOAD_SIZE, \
             or services.gradient.settings.maxSourceUploadSize on NixOS; the built-in \
             reverse proxy's client_max_body_size tracks it)."
        ),
        ConnectorError::Api { status, message } if status.as_u16() == 400 => format!(
            "Failed to upload {what}: the server could not read the upload (HTTP 400): {message}. \
             A reverse proxy may have truncated or rewritten the request body."
        ),
        ConnectorError::Unauthorized => format!(
            "Failed to upload {what}: not authenticated. Run `gradient login` and try again."
        ),
        other => format!("Failed to upload {what}: {other}"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;

    #[test]
    fn human_bytes_scales() {
        assert_eq!(human_bytes(512), "512 bytes");
        assert_eq!(human_bytes(208_685_264), "199.0 MiB (208685264 bytes)");
    }

    #[test]
    fn upload_error_413_is_actionable_and_hides_proxy_html() {
        let err = ConnectorError::Api {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            message: "<html><body>413 Request Entity Too Large</body></html>".to_string(),
        };
        let msg = upload_error_message(&err, 208_685_264, 53318, "acme");
        assert!(msg.contains("199.0 MiB"), "{msg}");
        assert!(msg.contains("53318 files"), "{msg}");
        assert!(msg.contains("GRADIENT_MAX_SOURCE_UPLOAD_SIZE"), "{msg}");
        assert!(!msg.contains("<html>"), "raw proxy HTML must be suppressed: {msg}");
    }

    #[test]
    fn upload_error_400_includes_server_message() {
        let err = ConnectorError::Api {
            status: StatusCode::BAD_REQUEST,
            message: "Failed to read nar: Error parsing `multipart/form-data` request".to_string(),
        };
        let msg = upload_error_message(&err, 1024, 3, "acme");
        assert!(msg.contains("HTTP 400"), "{msg}");
        assert!(msg.contains("multipart/form-data"), "{msg}");
    }

    #[test]
    fn upload_error_unauthorized_suggests_login() {
        let msg = upload_error_message(&ConnectorError::Unauthorized, 1024, 3, "acme");
        assert!(msg.contains("gradient login"), "{msg}");
    }
}
