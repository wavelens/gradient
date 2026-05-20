/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::config::*;
use crate::input::*;
use crate::output::Output;
use connector::*;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::exit;

pub async fn handle_build(
    target: Option<String>,
    system: Option<String>,
    organization: Option<String>,
    no_stream: bool,
    quiet: bool,
    out: Output,
) {
    let organization = organization
        .or_else(|| set_get_value(ConfigKey::SelectedOrganization, None, true))
        .unwrap_or_else(|| {
            if !quiet {
                out.progress(
                    "Organization must be set for build command. Use 'gradient organization select <name>' to set one.",
                );
            }
            exit(1);
        });

    let config = get_request_config(load_config()).unwrap_or_else(|_| {
        if !quiet {
            out.progress("Not configured. Use 'gradient config' to set server and auth token.");
        }
        exit(1);
    });

    let cwd = std::env::current_dir().unwrap_or_else(|e| {
        if !quiet {
            out.progress(format!("Failed to read current directory: {}", e));
        }
        exit(1);
    });

    let repo = git2::Repository::discover(&cwd).unwrap_or_else(|e| {
        if !quiet {
            out.progress(format!("Not in a git repository: {}", e));
        }
        exit(1);
    });

    let workdir = repo.workdir().map(Path::to_path_buf).unwrap_or_else(|| {
        if !quiet {
            out.progress("Bare repositories are not supported.");
        }
        exit(1);
    });

    let index = repo.index().unwrap_or_else(|e| {
        if !quiet {
            out.progress(format!("Failed to read git index: {}", e));
        }
        exit(1);
    });

    let mut entries: Vec<TrackedFile> = Vec::new();
    for entry in index.iter() {
        let path = match String::from_utf8(entry.path.clone()) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let abs = workdir.join(&path);
        if !abs.is_file() {
            continue;
        }
        match hash_file(&abs) {
            Ok((hash, size)) => entries.push(TrackedFile { path, hash, size, abs }),
            Err(e) => {
                if !quiet {
                    out.progress(format!("Failed to hash {}: {}", abs.display(), e));
                }
                exit(1);
            }
        }
    }

    if entries.is_empty() {
        if !quiet {
            out.progress("No tracked files to upload.");
        }
        exit(1);
    }

    if !quiet {
        out.human(format!("Sending manifest for {} tracked files...", entries.len()));
    }

    let manifest_req = build_requests::ManifestRequest {
        organization,
        files: entries
            .iter()
            .map(|e| build_requests::ManifestFile {
                path: e.path.clone(),
                hash: e.hash.clone(),
                size: e.size,
            })
            .collect(),
    };

    let manifest = match build_requests::post_manifest(config.clone(), manifest_req).await {
        Ok(r) if r.error => {
            if !quiet {
                out.progress("Manifest rejected by server.");
            }
            exit(1);
        }
        Ok(r) => r.message,
        Err(e) => {
            if !quiet {
                out.progress(format!("Failed to send manifest: {}", e));
            }
            exit(1);
        }
    };

    if !manifest.missing.is_empty() {
        if !quiet {
            out.human(format!(
                "Uploading {} missing blob(s) to session {}...",
                manifest.missing.len(),
                manifest.session
            ));
        }

        let missing: std::collections::HashSet<&str> =
            manifest.missing.iter().map(String::as_str).collect();
        let mut blobs: Vec<(String, Vec<u8>)> = Vec::with_capacity(missing.len());
        for entry in &entries {
            if !missing.contains(entry.hash.as_str()) {
                continue;
            }
            match std::fs::read(&entry.abs) {
                Ok(bytes) => blobs.push((entry.hash.clone(), bytes)),
                Err(e) => {
                    if !quiet {
                        out.progress(format!("Failed to read {}: {}", entry.abs.display(), e));
                    }
                    exit(1);
                }
            }
        }

        match build_requests::upload_blobs(config.clone(), manifest.session.clone(), blobs).await {
            Ok(r) if r.error => {
                if !quiet {
                    out.progress("Blob upload rejected by server.");
                }
                exit(1);
            }
            Ok(_) => {}
            Err(e) => {
                if !quiet {
                    out.progress(format!("Failed to upload blobs: {}", e));
                }
                exit(1);
            }
        }
    }

    let dispatch_req = build_requests::DispatchRequest { target, system };

    let dispatch = match build_requests::dispatch_build_request(
        config.clone(),
        manifest.session,
        dispatch_req,
    )
    .await
    {
        Ok(r) if r.error => {
            if !quiet {
                out.progress("Dispatch rejected by server.");
            }
            exit(1);
        }
        Ok(r) => r.message,
        Err(e) => {
            if !quiet {
                out.progress(format!("Failed to dispatch build request: {}", e));
            }
            exit(1);
        }
    };

    if quiet {
        out.human(format!("{}", dispatch.evaluation));
    } else {
        out.human(format!("Evaluation: {}", dispatch.evaluation));
        out.human(format!("Project:    {}", dispatch.project));
        out.human(format!("Commit:     {}", dispatch.commit));
    }

    if no_stream {
        return;
    }

    if !quiet {
        out.human("Streaming evaluation logs...");
    }

    if let Err(e) = evals::post_evaluation_builds(config, dispatch.evaluation).await
        && !quiet
    {
        out.progress(format!("Failed to stream evaluation logs: {}", e));
    }
}

struct TrackedFile {
    path: String,
    hash: String,
    size: i64,
    abs: PathBuf,
}

fn hash_file(path: &Path) -> std::io::Result<(String, i64)> {
    let mut hasher = blake3::Hasher::new();
    let mut reader = BufReader::new(std::fs::File::open(path)?);
    let mut buf = [0u8; 64 * 1024];
    let mut size: i64 = 0;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as i64;
    }
    Ok((hasher.finalize().to_hex().to_string(), size))
}
