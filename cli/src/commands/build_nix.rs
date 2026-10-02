/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::build::{BuildParams, TrackedFile, select_primary_entry_point};
use crate::config::{ConfigKey, load_config};
use crate::input::server_base;
use crate::output::{ExitKind, Output, to_exit_kind};
use connector::ConnectorError;
use connector::build_requests::BuildStartResponse;
use connector::evals::ArtefactTree;
use futures::StreamExt as _;
use harmonia_file_nar::NarByteStream;

const SOURCE_UPLOAD_CHUNK_SIZE: usize = 32 * 1024 * 1024;

/// The NAR is packed with the same serialiser the server is using. The store path then matches the
/// server's.
pub async fn start_via_nar(
    client: &connector::Client,
    project: &str,
    entries: &[TrackedFile],
    params: &BuildParams,
    quiet: bool,
    out: Output,
) -> BuildStartResponse {
    let staging = tempfile::tempdir()
        .unwrap_or_else(|e| out.err(ExitKind::Api, format!("Failed to create temp dir: {}", e)));

    for entry in entries {
        let dest = staging.path().join(&entry.path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).unwrap_or_else(|e| {
                out.err(
                    ExitKind::Api,
                    format!("Failed to stage {}: {}", entry.path, e),
                )
            });
        }
        std::fs::copy(&entry.abs, &dest).unwrap_or_else(|e| {
            out.err(
                ExitKind::Api,
                format!("Failed to stage {}: {}", entry.path, e),
            )
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

    let upload = blake3::hash(&nar).to_hex().to_string();
    let requests = client.build_requests();
    let total = nar.len() as u64;
    let mut offset = 0u64;
    while offset < total {
        let end = (offset as usize + SOURCE_UPLOAD_CHUNK_SIZE).min(nar.len());
        let chunk = nar[offset as usize..end].to_vec();
        match requests.upload_source_chunk(&upload, offset, chunk).await {
            Ok(received) if received > offset => offset = received,
            Ok(received) => out.err(
                ExitKind::Api,
                format!("Source upload stalled: server stayed at {received} of {total} bytes"),
            ),
            Err(e) => out.err(
                to_exit_kind(&e),
                upload_error_message(&e, nar_len, file_count, project),
            ),
        }
    }

    match requests
        .finalize_source(
            &upload,
            project,
            params.target.as_deref(),
            params.system.as_deref(),
            &params.overrides,
        )
        .await
    {
        Ok(d) => d,
        Err(e) => out.err(
            to_exit_kind(&e),
            upload_error_message(&e, nar_len, file_count, project),
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

fn upload_error_message(
    err: &ConnectorError,
    nar_len: usize,
    file_count: usize,
    project: &str,
) -> String {
    let what = format!(
        "source NAR {} from {file_count} files to project '{project}'",
        human_bytes(nar_len)
    );
    match err {
        ConnectorError::Api { status, .. } if status.as_u16() == 413 => format!(
            "Failed to upload {what}: the server rejected it as too large (HTTP 413). \
             Raise the source upload limit on the server (GRADIENT_HTTP_MAX_SOURCE_UPLOAD_SIZE, \
             or services.gradient.http.maxSourceUploadSize on NixOS; the built-in \
             reverse proxy's client_max_body_size tracks it)."
        ),
        ConnectorError::Api { status, message } if status.as_u16() == 400 => format!(
            "Failed to upload {what}: the server could not read the upload (HTTP 400): {message}. \
             A reverse proxy may have truncated or rewritten the request body."
        ),
        ConnectorError::Api { status, .. } if matches!(status.as_u16(), 502..=504) => format!(
            "Failed to upload {what}: the server closed the connection mid-upload (HTTP {}). \
             The source most likely exceeds the server's upload limit, which drops the \
             connection instead of returning a clean error - raise it \
             (GRADIENT_HTTP_MAX_SOURCE_UPLOAD_SIZE, or services.gradient.http.maxSourceUploadSize \
             on NixOS); otherwise the server may be down.",
            status.as_u16()
        ),
        ConnectorError::Unauthorized => format!(
            "Failed to upload {what}: not authenticated. Run `gradient login` and try again."
        ),
        other => format!("Failed to upload {what}: {other}"),
    }
}

pub async fn link_result(
    client: &connector::Client,
    started: &BuildStartResponse,
    tree: &ArtefactTree,
    target: Option<&str>,
    out: Output,
) {
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

    let (cache_opts, _netrc) = cache_substituter_opts(client, started, out).await;

    let mut cmd = tokio::process::Command::new("nix-store");
    cmd.args([
        "--realise",
        &realise_path,
        "--add-root",
        "result",
        "--indirect",
    ]);
    cmd.args(&cache_opts);
    match cmd.output().await {
        Ok(o) if o.status.success() => out.human(format!("result -> {realise_path}")),
        Ok(o) => out.err(
            ExitKind::Api,
            format!(
                "Could not materialise {realise_path}: neither the gradient cache nor your \
                 configured substituters can provide it.\n{}",
                String::from_utf8_lossy(&o.stderr).trim_end()
            ),
        ),
        Err(e) => out.err(ExitKind::Api, format!("Failed to run nix-store: {e}")),
    }
}

/// The returned netrc guard must outlive the nix process.
async fn cache_substituter_opts(
    client: &connector::Client,
    started: &BuildStartResponse,
    out: Output,
) -> (Vec<String>, Option<tempfile::NamedTempFile>) {
    let Some(cache) = started.cache.as_deref() else {
        out.human("Project has no cache; using local substituters only.");
        return (Vec::new(), None);
    };

    let base = server_base(out);
    let cache_url = format!("{}/cache/{}", base.trim_end_matches('/'), cache);
    let mut opts = vec!["--option".into(), "extra-substituters".into(), cache_url];

    if let Ok(public_key) = client.caches().public_key(cache).await {
        opts.push("--option".into());
        opts.push("extra-trusted-public-keys".into());
        opts.push(public_key);
    }

    let netrc = private_cache_netrc(client, cache, &base, out).await;
    if let Some(file) = &netrc {
        opts.push("--option".into());
        opts.push("netrc-file".into());
        opts.push(file.path().to_string_lossy().into_owned());
    }

    (opts, netrc)
}

async fn private_cache_netrc(
    client: &connector::Client,
    cache: &str,
    server: &str,
    out: Output,
) -> Option<tempfile::NamedTempFile> {
    if client.caches().public(cache).await.unwrap_or(false) {
        return None;
    }
    let token = load_config()
        .get(&ConfigKey::AuthToken)
        .and_then(|v| v.clone())
        .filter(|t| !t.is_empty())?;
    let host = crate::netrc::machine_host(server);
    let file = crate::netrc::temp_file(&host, &token)
        .unwrap_or_else(|e| out.err(ExitKind::Api, format!("Failed to write netrc: {e}")));
    Some(file)
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
        assert!(
            msg.contains("GRADIENT_HTTP_MAX_SOURCE_UPLOAD_SIZE"),
            "{msg}"
        );
        assert!(
            !msg.contains("<html>"),
            "raw proxy HTML must be suppressed: {msg}"
        );
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
