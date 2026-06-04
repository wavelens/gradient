/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::config::*;
use crate::input::client_from_config;
use crate::output::{Output, to_exit_kind};
use connector::build_requests::{BuildManifestRequest, DispatchRequest, ManifestFile};
use futures::StreamExt;
use futures::pin_mut;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::exit;

pub async fn handle_build(
    target: Option<String>,
    system: Option<String>,
    organization: Option<String>,
    background: bool,
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

    let client = client_from_config(out);

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
            Ok((hash, size)) => entries.push(TrackedFile {
                path,
                hash,
                size,
                abs,
            }),
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
        out.human(format!(
            "Sending manifest for {} tracked files...",
            entries.len()
        ));
    }

    let manifest_req = BuildManifestRequest {
        organization,
        files: entries
            .iter()
            .map(|e| ManifestFile {
                path: e.path.clone(),
                hash: e.hash.clone(),
                size: e.size,
            })
            .collect(),
    };

    let manifest = match client.build_requests().submit_manifest(manifest_req).await {
        Ok(m) => m,
        Err(e) => {
            if !quiet {
                out.progress(format!("Manifest rejected: {}", e));
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
        let mut form = reqwest::multipart::Form::new();
        for entry in &entries {
            if !missing.contains(entry.hash.as_str()) {
                continue;
            }
            match std::fs::read(&entry.abs) {
                Ok(bytes) => {
                    let part = reqwest::multipart::Part::bytes(bytes).file_name(entry.hash.clone());
                    form = form.part(entry.hash.clone(), part);
                }
                Err(e) => {
                    if !quiet {
                        out.progress(format!("Failed to read {}: {}", entry.abs.display(), e));
                    }
                    exit(1);
                }
            }
        }

        if let Err(e) = client
            .build_requests()
            .upload_blobs(&manifest.session, form)
            .await
        {
            if !quiet {
                out.progress(format!("Failed to upload blobs: {}", e));
            }
            exit(1);
        }
    }

    let dispatch = match client
        .build_requests()
        .dispatch(&manifest.session, DispatchRequest { target, system })
        .await
    {
        Ok(d) => d,
        Err(e) => {
            if !quiet {
                out.progress(format!("Failed to dispatch build request: {}", e));
            }
            exit(1);
        }
    };

    if background {
        out.ok(&dispatch);
        out.human(dispatch.evaluation.clone());
        return;
    }

    if quiet {
        out.human(dispatch.evaluation.clone());
    } else {
        out.ok(&dispatch);
        out.human(format!("Evaluation: {}", dispatch.evaluation));
        out.human(format!("Project:    {}", dispatch.project));
        out.human(format!("Commit:     {}", dispatch.commit));
    }

    if !quiet {
        out.human("Streaming evaluation logs...");
    }

    let evals = client.evals();
    let stream = match evals.stream_builds(&dispatch.evaluation).await {
        Ok(s) => s,
        Err(e) => {
            if !quiet {
                out.progress(format!("Failed to stream evaluation logs: {}", e));
            }
            return;
        }
    };

    pin_mut!(stream);
    while let Some(item) = stream.next().await {
        match item {
            Ok(line) => {
                if out.is_json() {
                    let env = serde_json::json!({"error": false, "message": line});
                    println!("{}", env);
                } else {
                    print!("{}", line);
                }
            }
            Err(e) => out.err(to_exit_kind(&e), e),
        }
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
