/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::attr_spec;
use crate::error::CliError;
use crate::input::{client_from_config, selected_project};
use crate::output::{ExitKind, Output};
use crate::tui::watch::eval_is_terminal;
use connector::ConnectorError;
#[cfg(not(feature = "nix"))]
use connector::api::ArtefactTree;
use connector::api::{DispatchResponse, EvaluationResponse, EvaluationStatus, RunOptions};
#[cfg(not(feature = "nix"))]
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::exit;

pub(crate) struct BuildParams {
    pub target: Option<String>,
    pub system: Option<String>,
    pub overrides: Vec<(String, String)>,
    pub run: Option<RunOptions>,
}

pub async fn handle_build(
    params: BuildParams,
    background: bool,
    quiet: bool,
    no_link: bool,
    out: Output,
) {
    let (client, started) = submit_build_request(params, quiet, out).await;
    let evaluation = started.evaluation.to_string();

    if background {
        out.ok(&started);
        out.human(evaluation.clone());
        return;
    }

    if quiet {
        out.human(evaluation.clone());
    } else {
        out.ok(&started);
        out.human(format!("Evaluation: {}", evaluation));
        out.human(format!("Task:    {}", started.task));
        out.human(format!("Commit:     {}", started.commit));
    }

    if !quiet {
        out.human("Streaming evaluation logs...");
    }

    if let Err(e) = crate::commands::logstream::stream_eval_logs(&client, &evaluation, out).await {
        out.fail(e);
    }

    let eval = wait_for_terminal(&client, &evaluation, out).await;
    if let Some(failure) = failure_message(eval.status, eval.error.as_deref()) {
        out.err(ExitKind::Api, failure);
    }

    if no_link {
        return;
    }

    let tree = client
        .evals()
        .artefacts(&evaluation)
        .await
        .unwrap_or_else(|e| out.err(ExitKind::Api, format!("Could not fetch artefacts: {e}")));

    #[cfg(feature = "nix")]
    crate::commands::build_nix::link_results(&client, &started, &tree, out).await;
    #[cfg(not(feature = "nix"))]
    download_result_dirs(&client, &tree, out).await;
}

pub(crate) async fn submit_build_request(
    mut params: BuildParams,
    quiet: bool,
    out: Output,
) -> (connector::Client, DispatchResponse) {
    // A missing server or session must surface before the project check. An unconfigured first
    // invocation then points at `gradient login` (#498).
    let client = client_from_config(out);
    if let Err(ConnectorError::Unauthorized) = client.user().get().await {
        out.fail(CliError::NotLoggedIn);
    }

    let project = selected_project(out);

    params.target = params.target.take().map(|raw| {
        let system = params
            .system
            .clone()
            .unwrap_or_else(attr_spec::default_nix_system);
        let category = match params.run {
            Some(_) => "apps",
            None => "packages",
        };
        let normalized = normalize_target(&raw, &system, category);
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

    let started = upload_and_start(&client, &project, &entries, &params, quiet, out).await;
    (client, started)
}

pub(crate) fn failure_message(status: EvaluationStatus, error: Option<&str>) -> Option<String> {
    if status == EvaluationStatus::Completed {
        return None;
    }

    let status = format!("Build did not complete (status: {status}).");
    Some(match error.filter(|s| !s.is_empty()) {
        Some(error) => format!("Evaluation error: {error}\n{status}"),
        None => status,
    })
}

pub(crate) fn result_link_name(index: usize) -> String {
    match index {
        0 => "result".to_owned(),
        n => format!("result-{n}"),
    }
}

async fn upload_and_start(
    client: &connector::Client,
    project: &str,
    entries: &[TrackedFile],
    params: &BuildParams,
    quiet: bool,
    out: Output,
) -> DispatchResponse {
    #[cfg(feature = "nix")]
    {
        crate::commands::build_nix::start_via_nar(client, project, entries, params, quiet, out)
            .await
    }
    #[cfg(not(feature = "nix"))]
    {
        start_via_manifest(client, project, entries, params, quiet, out).await
    }
}

#[cfg(not(feature = "nix"))]
async fn start_via_manifest(
    client: &connector::Client,
    project: &str,
    entries: &[TrackedFile],
    params: &BuildParams,
    quiet: bool,
    out: Output,
) -> DispatchResponse {
    use connector::api::{DispatchRequest, InputOverrideBody, ManifestEntry, ManifestRequest};

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

    let manifest_req = ManifestRequest {
        project: project.to_owned(),
        files: hashed
            .iter()
            .map(|(path, hash, size)| ManifestEntry {
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
            .upload_blobs(&manifest.session.to_string(), form)
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

    let input_overrides = params
        .overrides
        .iter()
        .map(|(input_name, url)| InputOverrideBody {
            input_name: input_name.clone(),
            url: url.clone(),
        })
        .collect();

    match client
        .build_requests()
        .start(
            &manifest.session.to_string(),
            DispatchRequest {
                target: params.target.clone(),
                system: params.system.clone(),
                input_overrides,
                run: params.run.clone(),
            },
        )
        .await
    {
        Ok(d) => d,
        Err(e) => {
            if !quiet {
                out.progress(format!("Failed to start build request: {}", e));
            }
            exit(1);
        }
    }
}

async fn wait_for_terminal(
    client: &connector::Client,
    eval_id: &str,
    out: Output,
) -> EvaluationResponse {
    loop {
        match client.evals().get(eval_id).await {
            Ok(e) if eval_is_terminal(e.status) => return e,
            Ok(_) => {}
            Err(e) => out.err(ExitKind::Api, format!("Status poll failed: {e}")),
        }

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// The flake ref is always the uploaded repo. A leading local ref before `#` is dropped, and a bare
/// attr is qualified like `nix build`.
fn normalize_target(raw: &str, system: &str, category: &str) -> String {
    raw.split(',')
        .map(|pat| normalize_installable(pat.trim(), system, category))
        .collect::<Vec<_>>()
        .join(",")
}

fn normalize_installable(pat: &str, system: &str, category: &str) -> String {
    let (excl, body) = pat.strip_prefix('!').map(|r| ("!", r)).unwrap_or(("", pat));

    match body.split_once('#') {
        Some(("" | "." | "./", attr)) => {
            attr_spec::qualify_attr(&format!("{excl}{attr}"), system, category)
        }
        _ => pat.to_string(),
    }
}

const REMOTE_OVERRIDE_SCHEMES: &[&str] = &[
    "github:",
    "gitlab:",
    "sourcehut:",
    "git+ssh://",
    "git+https://",
    "git+http://",
    "git://",
    "https://",
    "http://",
    "flake:",
];

pub(crate) fn parse_overrides(raw: &[String]) -> Result<Vec<(String, String)>, String> {
    let (pairs, []) = raw.as_chunks::<2>() else {
        return Err("--override-input needs INPUT and FLAKE".into());
    };
    let mut out = Vec::with_capacity(pairs.len());
    for [name, ref_] in pairs {
        let (name, ref_) = (name.clone(), ref_.clone());
        let mut name_chars = name.chars();
        let name_ok = matches!(name_chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
            && name_chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if !name_ok {
            return Err(format!(
                "override-input '{name}': input name must match ^[A-Za-z_][A-Za-z0-9_-]*$"
            ));
        }

        let ok = REMOTE_OVERRIDE_SCHEMES.iter().any(|s| ref_.starts_with(s))
            || ref_.starts_with("path:/nix/store/");
        if !ok {
            return Err(format!(
                "override-input '{name}': '{ref_}' is not a remote flake ref. \
                 gradient build evaluates on the server, so local paths are not \
                 supported; use github:/gitlab:/git+ssh://flake:/path:/nix/store/... ."
            ));
        }
        out.push((name, ref_));
    }
    Ok(out)
}

#[cfg(not(feature = "nix"))]
async fn download_result_dirs(client: &connector::Client, tree: &ArtefactTree, out: Output) {
    if tree.entry_points.is_empty() {
        out.human("No outputs to download.");
        return;
    }

    for (index, ep) in tree.entry_points.iter().enumerate() {
        download_result_dir(client, ep, &result_link_name(index), out).await;
    }
}

#[cfg(not(feature = "nix"))]
async fn download_result_dir(
    client: &connector::Client,
    ep: &connector::api::EntryPointArtefacts,
    dir_name: &str,
    out: Output,
) {
    use crate::commands::download::{product_filename, safe_relative_name};

    let dir = PathBuf::from(dir_name);
    let mut wrote = 0usize;
    for output in &ep.outputs {
        for product in &output.products {
            let name = product_filename(product);
            let bytes = match client
                .builds()
                .download_file(&ep.build_id.to_string(), &name)
                .await
            {
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
        out.human(format!(
            "No build products of {} to place in {dir_name}/.",
            ep.attr
        ));
    } else {
        out.human(format!(
            "Wrote {wrote} product(s) of {} to {dir_name}/",
            ep.attr
        ));
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
    use super::{failure_message, normalize_installable, normalize_target, result_link_name};
    use connector::api::EvaluationStatus;

    #[test]
    fn only_a_completed_evaluation_succeeds() {
        assert_eq!(failure_message(EvaluationStatus::Completed, None), None);
        for status in [EvaluationStatus::Failed, EvaluationStatus::Aborted] {
            assert!(failure_message(status, None).is_some(), "{status}");
        }
    }

    #[test]
    fn failure_message_carries_the_evaluation_error() {
        let message = failure_message(EvaluationStatus::Failed, Some("attr missing")).unwrap();
        assert!(message.contains("attr missing"), "{message}");
        assert!(message.contains("Failed"), "{message}");
    }

    #[test]
    fn result_links_follow_nix_build_numbering() {
        let names: Vec<_> = (0..3).map(result_link_name).collect();
        assert_eq!(names, ["result", "result-1", "result-2"]);
    }

    #[test]
    fn parse_overrides_accepts_remote_refs() {
        let raw = vec![
            "nixpkgs".into(),
            "github:NixOS/nixpkgs/nixos-unstable".into(),
            "priv".into(),
            "git+ssh://git@h/x.git".into(),
        ];
        let out = super::parse_overrides(&raw).unwrap();
        assert_eq!(
            out,
            vec![
                (
                    "nixpkgs".into(),
                    "github:NixOS/nixpkgs/nixos-unstable".into()
                ),
                ("priv".into(), "git+ssh://git@h/x.git".into())
            ]
        );
    }

    #[test]
    fn parse_overrides_rejects_local_paths() {
        for bad in ["/home/u/np", "./np", "~/np", "np", "path:/home/u/np"] {
            let raw = vec!["nixpkgs".into(), bad.to_string()];
            assert!(
                super::parse_overrides(&raw).is_err(),
                "{bad} must be rejected"
            );
        }
    }

    #[test]
    fn parse_overrides_accepts_store_path() {
        let raw = vec![
            "a".into(),
            "path:/nix/store/xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx-src".into(),
        ];
        assert!(super::parse_overrides(&raw).is_ok());
    }

    #[test]
    fn parse_overrides_rejects_bad_input_name() {
        for bad in ["1bad", "has space", "has.dot", ""] {
            let raw = vec![bad.to_string(), "github:x/y".into()];
            assert!(
                super::parse_overrides(&raw).is_err(),
                "name '{bad}' must be rejected"
            );
        }
    }

    #[test]
    fn bare_installable_qualifies_to_packages() {
        assert_eq!(
            normalize_installable(".#uxc", "x86_64-linux", "packages"),
            "packages.x86_64-linux.uxc"
        );
        assert_eq!(
            normalize_installable("#uxc", "aarch64-darwin", "packages"),
            "packages.aarch64-darwin.uxc"
        );
        assert_eq!(
            normalize_installable(".#", "x86_64-linux", "packages"),
            "packages.x86_64-linux.*"
        );
    }

    #[test]
    fn qualified_and_wildcard_targets_pass_through() {
        assert_eq!(
            normalize_installable("packages.x86_64-linux.*", "x86_64-linux", "packages"),
            "packages.x86_64-linux.*"
        );
        assert_eq!(
            normalize_installable("checks.x86_64-linux.*", "x86_64-linux", "packages"),
            "checks.x86_64-linux.*"
        );
        assert_eq!(normalize_installable("*", "x86_64-linux", "packages"), "*");
        assert_eq!(
            normalize_installable(".#packages.aarch64-linux.uxc", "x86_64-linux", "packages"),
            "packages.aarch64-linux.uxc"
        );
        assert_eq!(
            normalize_installable(".#nixosConfigurations.foo", "x86_64-linux", "packages"),
            "nixosConfigurations.foo"
        );
    }

    #[test]
    fn exclusions_and_comma_lists_preserved() {
        assert_eq!(
            normalize_installable("!.#uxc", "x86_64-linux", "packages"),
            "!packages.x86_64-linux.uxc"
        );
        assert_eq!(
            normalize_installable("!nixosConfigurations.foo", "x86_64-linux", "packages"),
            "!nixosConfigurations.foo"
        );
        assert_eq!(
            normalize_target(".#uxc,.#cli", "x86_64-linux", "packages"),
            "packages.x86_64-linux.uxc,packages.x86_64-linux.cli"
        );
    }

    #[test]
    fn run_targets_qualify_to_apps() {
        assert_eq!(
            normalize_target(".#default,.#frontend", "x86_64-linux", "apps"),
            "apps.x86_64-linux.default,apps.x86_64-linux.frontend"
        );
    }
}
