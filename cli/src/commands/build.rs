/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::config::*;
use crate::input::client_from_config;
use crate::output::{ExitKind, Output, to_exit_kind};
use connector::ConnectorError;
use connector::build_requests::DispatchResponse;
use connector::evals::{ArtefactTree, EntryPointArtefacts, EvaluationResponse};
use futures::StreamExt;
use futures::pin_mut;
#[cfg(not(feature = "nix"))]
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::exit;

pub async fn handle_build(
    target: Option<String>,
    system: Option<String>,
    organization: Option<String>,
    background: bool,
    quiet: bool,
    no_link: bool,
    out: Output,
) {
    // Surface a missing server / session before the org check so an unconfigured
    // first run points at `gradient login` rather than org selection (#498).
    let client = client_from_config(out);
    if let Err(ConnectorError::Unauthorized) = client.user().get().await {
        out.err(
            ExitKind::Unauthorized,
            "Not authenticated: your gradient session is missing or expired. \
             Run `gradient login <url>` and try again.",
        );
    }

    let organization = organization
        .or_else(|| set_get_value(ConfigKey::SelectedOrganization, None, true))
        .unwrap_or_else(|| {
            if !quiet {
                out.progress(
                    "Organization must be set for build command. Use 'gradient organization select <name>' \
                     (it is selected automatically on `gradient login`).",
                );
            }
            exit(1);
        });

    // Accept `nix build`-style installables (`.#uxc`) and translate them into
    // gradient's attr-path wildcard language before dispatch and result linking.
    let target = target.map(|raw| {
        let system = system.clone().unwrap_or_else(default_nix_system);
        let normalized = normalize_target(&raw, &system);
        if !quiet && normalized != raw {
            out.progress(format!("Building '{}' (from '{}')", normalized, raw));
        }
        normalized
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
        entries.push(TrackedFile { path, abs });
    }

    if entries.is_empty() {
        if !quiet {
            out.progress("No tracked files to upload.");
        }
        exit(1);
    }

    let dispatch = upload_and_dispatch(
        &client,
        &organization,
        &entries,
        target.clone(),
        system.clone(),
        quiet,
        out,
    )
    .await;

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

    if no_link {
        return;
    }

    let eval = wait_for_terminal(&client, &dispatch.evaluation, out).await;
    let status = eval.as_ref().map(|e| e.status.clone()).unwrap_or_default();
    if status != "Completed" {
        if !quiet {
            if let Some(err) = eval
                .as_ref()
                .and_then(|e| e.error.as_deref())
                .filter(|s| !s.is_empty())
            {
                out.human(format!("Evaluation error: {err}"));
            }

            out.human(format!(
                "Build did not complete (status: {status}); skipping result."
            ));
        }
        return;
    }

    let tree = match client.evals().artefacts(&dispatch.evaluation).await {
        Ok(t) => t,
        Err(e) => {
            if !quiet {
                out.progress(format!("Could not fetch artefacts: {}", e));
            }
            return;
        }
    };

    #[cfg(feature = "nix")]
    crate::commands::build_nix::link_result(&dispatch, &tree, target.as_deref(), out).await;
    #[cfg(not(feature = "nix"))]
    download_result_dir(&client, &tree, target.as_deref(), out).await;
}

async fn upload_and_dispatch(
    client: &connector::Client,
    organization: &str,
    entries: &[TrackedFile],
    target: Option<String>,
    system: Option<String>,
    quiet: bool,
    out: Output,
) -> DispatchResponse {
    #[cfg(feature = "nix")]
    {
        crate::commands::build_nix::dispatch_via_nar(
            client,
            organization,
            target,
            system,
            entries,
            quiet,
            out,
        )
        .await
    }
    #[cfg(not(feature = "nix"))]
    {
        dispatch_via_manifest(client, organization, entries, target, system, quiet, out).await
    }
}

#[cfg(not(feature = "nix"))]
async fn dispatch_via_manifest(
    client: &connector::Client,
    organization: &str,
    entries: &[TrackedFile],
    target: Option<String>,
    system: Option<String>,
    quiet: bool,
    out: Output,
) -> DispatchResponse {
    use connector::build_requests::{BuildManifestRequest, DispatchRequest, ManifestFile};

    if !quiet {
        out.human(format!(
            "Sending manifest for {} tracked files...",
            entries.len()
        ));
    }

    let hashed: Vec<(String, String, i64)> = entries
        .iter()
        .map(|e| match hash_file(&e.abs) {
            Ok((hash, size)) => (e.path.clone(), hash, size),
            Err(err) => {
                if !quiet {
                    out.progress(format!("Failed to hash {}: {}", e.abs.display(), err));
                }
                exit(1);
            }
        })
        .collect();

    let manifest_req = BuildManifestRequest {
        organization: organization.to_owned(),
        files: hashed
            .iter()
            .map(|(path, hash, size)| ManifestFile {
                path: path.clone(),
                hash: hash.clone(),
                size: *size,
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
        let abs_by_path: std::collections::HashMap<&str, &PathBuf> =
            entries.iter().map(|e| (e.path.as_str(), &e.abs)).collect();
        let mut form = reqwest::multipart::Form::new();
        for (path, hash, _) in &hashed {
            if !missing.contains(hash.as_str()) {
                continue;
            }
            let abs = abs_by_path[path.as_str()];
            match std::fs::read(abs) {
                Ok(bytes) => {
                    let part = reqwest::multipart::Part::bytes(bytes).file_name(hash.clone());
                    form = form.part(hash.clone(), part);
                }
                Err(e) => {
                    if !quiet {
                        out.progress(format!("Failed to read {}: {}", abs.display(), e));
                    }
                    exit(1);
                }
            }
        }

        match client
            .build_requests()
            .upload_blobs(&manifest.session, form)
            .await
        {
            Ok(resp) => {
                if !quiet {
                    out.human(format!("Uploaded {} blob(s).", resp.uploaded));
                }
            }
            Err(e) => {
                if !quiet {
                    out.progress(format!("Failed to upload blobs: {}", e));
                }
                exit(1);
            }
        }
    }

    match client
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
    }
}

/// Poll the evaluation until it reaches a terminal status, returning it.
/// `None` means the poll failed (already reported to `out`).
async fn wait_for_terminal(
    client: &connector::Client,
    eval_id: &str,
    out: Output,
) -> Option<EvaluationResponse> {
    loop {
        match client.evals().get(eval_id).await {
            Ok(e) => {
                if matches!(e.status.as_str(), "Completed" | "Failed" | "Aborted") {
                    return Some(e);
                }
            }
            Err(e) => {
                out.progress(format!("Status poll failed: {}", e));
                return None;
            }
        }

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// Flake-output categories that are already gradient attr-path syntax; a target
/// under one of these passes through, anything else is treated as a bare package.
const OUTPUT_CATEGORIES: &[&str] = &[
    "packages",
    "legacyPackages",
    "checks",
    "devShells",
    "apps",
    "nixosConfigurations",
    "darwinConfigurations",
    "homeConfigurations",
    "hydraJobs",
    "formatter",
    "bundlers",
];

/// The host's Nix system double (`x86_64-linux`, `aarch64-darwin`, ...), used to
/// qualify a bare `.#uxc` as `packages.<system>.uxc` the way `nix build` does.
fn default_nix_system() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    format!("{}-{}", std::env::consts::ARCH, os)
}

/// Translate a `nix build`-style installable into gradient's `.`-separated
/// attr-path wildcard language. `gradient build .#uxc` mirrors `nix build .#uxc`:
/// the flake ref is always the uploaded repo, so drop a leading local ref
/// (`.`/empty) before `#` and qualify a bare attr as `packages.<system>.<attr>`.
/// Fully-qualified paths and `*`/`#` wildcards (`packages.x86_64-linux.#`) and
/// exclusions pass through untouched.
fn normalize_target(raw: &str, system: &str) -> String {
    raw.split(',')
        .map(|pat| normalize_installable(pat.trim(), system))
        .collect::<Vec<_>>()
        .join(",")
}

fn normalize_installable(pat: &str, system: &str) -> String {
    let (excl, body) = pat
        .strip_prefix('!')
        .map(|r| ("!", r))
        .unwrap_or(("", pat));

    let attr = match body.split_once('#') {
        Some(("" | "." | "./", attr)) => attr,
        _ => return pat.to_string(),
    };

    if attr.is_empty() {
        return format!("{excl}packages.{system}.#");
    }

    let head = attr.split('.').next().unwrap_or("");
    if head == "*" || head == "#" || OUTPUT_CATEGORIES.contains(&head) {
        format!("{excl}{attr}")
    } else {
        format!("{excl}packages.{system}.{attr}")
    }
}

/// Pick the entry point matching `target` (exact or suffix), else the first.
pub(crate) fn select_primary_entry_point<'a>(
    tree: &'a ArtefactTree,
    target: Option<&str>,
) -> Option<&'a EntryPointArtefacts> {
    if let Some(t) = target.filter(|t| *t != "*")
        && let Some(ep) = tree
            .entry_points
            .iter()
            .find(|e| e.attr == t || e.attr.ends_with(t))
    {
        return Some(ep);
    }

    tree.entry_points.first()
}

#[cfg(not(feature = "nix"))]
async fn download_result_dir(
    client: &connector::Client,
    tree: &ArtefactTree,
    target: Option<&str>,
    out: Output,
) {
    use crate::commands::download::{product_filename, safe_relative_name};

    let Some(ep) = select_primary_entry_point(tree, target) else {
        out.human("No outputs to download.");
        return;
    };

    let dir = PathBuf::from("result");
    let mut wrote = 0usize;
    for output in &ep.outputs {
        for product in &output.products {
            let name = product_filename(product);
            let bytes = match client.builds().download_file(&ep.build_id, &name).await {
                Ok(b) => b,
                Err(e) => {
                    out.progress(format!("Failed to download {}: {}", name, e));
                    continue;
                }
            };
            let dest = dir.join(safe_relative_name(&name));
            if let Some(parent) = dest.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = std::fs::write(&dest, bytes) {
                out.progress(format!("Failed to write {}: {}", dest.display(), e));
                continue;
            }
            wrote += 1;
        }
    }

    if wrote == 0 {
        out.human("No build products to place in result/.");
    } else {
        out.human(format!("Wrote {} product(s) to result/", wrote));
    }
}

pub(crate) struct TrackedFile {
    pub(crate) path: String,
    pub(crate) abs: PathBuf,
}

#[cfg(not(feature = "nix"))]
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

#[cfg(test)]
mod tests {
    use super::{normalize_installable, normalize_target};

    #[test]
    fn bare_installable_qualifies_to_packages() {
        assert_eq!(
            normalize_installable(".#uxc", "x86_64-linux"),
            "packages.x86_64-linux.uxc"
        );
        assert_eq!(
            normalize_installable("#uxc", "aarch64-darwin"),
            "packages.aarch64-darwin.uxc"
        );
        // `.#` alone builds every package, like `nix build .#` picks the default.
        assert_eq!(
            normalize_installable(".#", "x86_64-linux"),
            "packages.x86_64-linux.#"
        );
    }

    #[test]
    fn qualified_and_wildcard_targets_pass_through() {
        // gradient's own trailing `#` wildcard segment must survive untouched.
        assert_eq!(
            normalize_installable("packages.x86_64-linux.#", "x86_64-linux"),
            "packages.x86_64-linux.#"
        );
        assert_eq!(
            normalize_installable("checks.x86_64-linux.*", "x86_64-linux"),
            "checks.x86_64-linux.*"
        );
        assert_eq!(normalize_installable("*", "x86_64-linux"), "*");
        assert_eq!(
            normalize_installable(".#packages.aarch64-linux.uxc", "x86_64-linux"),
            "packages.aarch64-linux.uxc"
        );
        assert_eq!(
            normalize_installable(".#nixosConfigurations.foo", "x86_64-linux"),
            "nixosConfigurations.foo"
        );
    }

    #[test]
    fn exclusions_and_comma_lists_preserved() {
        assert_eq!(
            normalize_installable("!.#uxc", "x86_64-linux"),
            "!packages.x86_64-linux.uxc"
        );
        assert_eq!(
            normalize_installable("!nixosConfigurations.foo", "x86_64-linux"),
            "!nixosConfigurations.foo"
        );
        assert_eq!(
            normalize_target(".#uxc,.#cli", "x86_64-linux"),
            "packages.x86_64-linux.uxc,packages.x86_64-linux.cli"
        );
    }
}
