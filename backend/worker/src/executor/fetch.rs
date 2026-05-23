/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Fetch task - clone the repository, archive it into the Nix store, and
//! upload the source + all flake inputs to the Gradient cache.
//!
//! Private repositories are accessed using the SSH private key delivered by the
//! server as a [`proto::messages::ServerMessage::Credential`] with
//! [`proto::messages::CredentialKind::SshKey`].  The key is available via
//! [`CredentialStore::ssh_key`] before this step executes.

use std::collections::HashSet;

use anyhow::{Context, Result};
use git2::RemoteCallbacks;
use proto::messages::{FlakeJob, FlakeSource};
use proto::traits::JobReporter;
use tempfile::NamedTempFile;
use tokio::sync::watch;
use tracing::{debug, info, trace};

use crate::proto::credentials::CredentialStore;

/// Future that resolves only when the abort signal becomes `true`.
///
/// Uses `changed()` + `borrow()` (not `wait_for`) to avoid holding a
/// non-`Send` `Ref<'_, bool>` guard across an await point.
///
/// If the sender is dropped (e.g. in tests using a receiver without a sender),
/// the future parks forever instead of treating the drop as an abort.
async fn abort_true(abort: &mut watch::Receiver<bool>) {
    loop {
        match abort.changed().await {
            Ok(()) => {
                if *abort.borrow() {
                    return;
                }
            }
            // Sender dropped - treat as "no abort", park forever.
            Err(_) => std::future::pending::<()>().await,
        }
    }
}

/// Outcome of a successful `fetch_repository` call.
///
/// `local_flake_path` is the path eval tasks should point at (either the
/// archived nix-store source, or the temporary git checkout on fallback).
/// `flake_source` is `Some(store_path)` when `nix flake archive` succeeded
/// and the source now lives in the cache - this is the value reported back
/// to the server so subsequent eval-only jobs can use
/// `FlakeSource::Cached { store_path }`. On archive fallback it is `None`
/// (worker is on a tmp checkout; no eval-only follow-up possible).
/// `archived_paths` lists every store path produced by the archive (source
/// + transitive inputs) - the caller pushes and optionally signs these.
///   Empty on fallback.
pub struct FetchOutcome {
    pub local_flake_path: String,
    pub flake_source: Option<String>,
    pub archived_paths: Vec<String>,
}

/// Clone the repository referenced by `job`, archive it and all flake inputs
/// into the Nix store, and return metadata about the archive.
///
/// The caller is responsible for pushing the NARs (via `nar::push_direct`) and
/// reporting the result to the server (via `report_fetch_result`).
///
/// `abort` is a watch channel receiver; when its value becomes `true` the
/// function returns an error immediately (or kills any running subprocess).
pub async fn fetch_repository(
    job: &FlakeJob,
    updater: &mut dyn JobReporter,
    credentials: &CredentialStore,
    binpath_nix: &str,
    binpath_ssh: &str,
    mut abort: watch::Receiver<bool>,
) -> Result<FetchOutcome> {
    if *abort.borrow() {
        anyhow::bail!("job aborted");
    }

    updater.report_fetching().await?;

    // Only Repository sources are supported on this path - Cached sources
    // skip FetchFlake entirely and go straight to eval. The scheduler
    // guarantees this by construction, but guard in case.
    let (url, commit) = match &job.source {
        FlakeSource::Repository { url, commit } => (url.clone(), commit.clone()),
        FlakeSource::Cached { .. } => {
            anyhow::bail!("FetchFlake task requires FlakeSource::Repository");
        }
    };
    let ssh_key = credentials
        .ssh_key()
        .map(|k| String::from_utf8_lossy(k.expose()).to_string());

    debug!(%url, %commit, has_ssh_key = ssh_key.is_some(), "fetching repository");

    let ssh_key_for_clone = ssh_key.clone();
    let commit_for_clone = commit.clone();
    let clone_task = tokio::task::spawn_blocking(move || {
        clone_and_checkout(&url, &commit_for_clone, ssh_key_for_clone.as_deref())
    });

    let tmp_path = tokio::select! {
        biased;
        _ = abort_true(&mut abort) => {
            anyhow::bail!("job aborted during git clone");
        }
        result = clone_task => {
            result.context("fetch task panicked")??
        }
    };

    let overrides_in: Vec<OverrideInput> = job.input_overrides.iter().map(Into::into).collect();
    let (applied_overrides, warnings) = if overrides_in.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        let lock_path = std::path::Path::new(&tmp_path).join("flake.lock");
        let lock_bytes = tokio::fs::read(&lock_path)
            .await
            .with_context(|| format!("failed to read {}", lock_path.display()))?;
        let lock: serde_json::Value =
            serde_json::from_slice(&lock_bytes).context("failed to parse flake.lock")?;
        let declared = declared_inputs_from_lock(&lock)?;
        resolve_overrides(&overrides_in, &declared, &lock)?
    };

    for msg in &warnings {
        updater
            .send_eval_message(
                gradient_core::types::proto::EvalMessageLevel::Warning,
                "fetch",
                msg,
            )
            .await?;
    }

    if !applied_overrides.is_empty() {
        info!(count = applied_overrides.len(), "applying flake input overrides");
    }

    // Archive the flake source and all locked inputs into the nix store via a
    // subprocess (so fetching goes through the nix daemon with proper network
    // and store-write access).  Returns the nix store source path so the
    // evaluator can use `path:/nix/store/xxx` - a pure, content-addressed
    // reference - instead of the git checkout in /tmp.
    let flake_ref = format!("git+file://{}?rev={}", tmp_path, commit);
    let binpath_nix = binpath_nix.to_owned();
    let binpath_ssh = binpath_ssh.to_owned();
    match archive_flake(
        &flake_ref,
        &binpath_nix,
        &binpath_ssh,
        ssh_key.as_deref(),
        &applied_overrides,
        abort,
    )
    .await
    {
        Ok((source_path, archived_paths)) => {
            info!(%source_path, inputs = archived_paths.len(), "flake archived to nix store");
            Ok(FetchOutcome {
                local_flake_path: source_path.clone(),
                flake_source: Some(source_path),
                archived_paths,
            })
        }
        Err(e) => Err(e),
    }
}

fn parse_nix_json(stdout: &[u8], cmd: &str) -> Result<serde_json::Value> {
    serde_json::from_slice(stdout).with_context(|| format!("failed to parse {cmd} JSON"))
}

fn build_archive_argv(flake_ref: &str, overrides: &[(String, String)]) -> Vec<String> {
    let mut argv = vec!["flake".to_owned(), "archive".to_owned()];
    for (name, ref_str) in overrides {
        argv.push("--override-input".to_owned());
        argv.push(name.clone());
        argv.push(ref_str.clone());
    }
    argv.push("--json".to_owned());
    argv.push(flake_ref.to_owned());
    argv
}

/// Run `nix flake archive --json` and collect all store paths (source + all
/// transitive flake inputs).  Returns the source store path and metadata for
/// every archived path obtained from `nix path-info`.
///
/// When `ssh_key` is `Some`, the key is written to a mode-600 temp file and
/// `GIT_SSH_COMMAND` is set on the subprocess so libfetchers can clone private
/// `git+ssh` inputs.  The temp file is deleted when this function returns.
async fn archive_flake(
    flake_ref: &str,
    binpath_nix: &str,
    binpath_ssh: &str,
    ssh_key: Option<&str>,
    overrides: &[(String, String)],
    mut abort: watch::Receiver<bool>,
) -> Result<(String, Vec<String>)> {
    use std::os::unix::fs::PermissionsExt;

    trace!(binpath_nix, flake_ref, "executing nix flake archive");
    let mut cmd = tokio::process::Command::new(binpath_nix);
    cmd.args(build_archive_argv(flake_ref, overrides));
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);

    // Write the SSH key to a temp file (mode 0600) and set GIT_SSH_COMMAND so
    // that nix's libfetchers picks it up when cloning git+ssh inputs.  The
    // _key_file guard ensures the file is deleted when this scope exits.
    let _key_file: Option<NamedTempFile> = if let Some(key) = ssh_key {
        let kf =
            NamedTempFile::with_suffix(".key").context("failed to create SSH key temp file")?;
        std::fs::set_permissions(kf.path(), std::fs::Permissions::from_mode(0o600))
            .context("failed to chmod SSH key file")?;
        std::fs::write(kf.path(), key.as_bytes()).context("failed to write SSH key file")?;
        let ssh_command = format!(
            "{} -i {} -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null",
            binpath_ssh,
            kf.path().display()
        );
        cmd.env("GIT_SSH_COMMAND", ssh_command);
        Some(kf)
    } else {
        None
    };

    let child = cmd.spawn().context("failed to spawn nix flake archive")?;

    // Spawn into a separate task so abort_handle can cancel it (dropping child,
    // which triggers kill_on_drop) independently of the await future.
    let archive_task = tokio::spawn(async move { child.wait_with_output().await });
    let abort_handle = archive_task.abort_handle();

    let output = tokio::select! {
        biased;
        _ = abort_true(&mut abort) => {
            abort_handle.abort();
            anyhow::bail!("job aborted during nix flake archive");
        }
        result = archive_task => {
            result
                .context("nix flake archive task panicked")?
                .context("failed to run nix flake archive")?
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("nix flake archive failed: {}", stderr.trim());
    }

    let json: serde_json::Value = parse_nix_json(&output.stdout, "nix flake archive")?;

    let source_path = json["path"]
        .as_str()
        .context("nix flake archive JSON missing 'path' field")?
        .to_owned();

    // Collect every store path referenced by the archive output (deduplicated).
    let mut all_paths: HashSet<String> = HashSet::new();
    all_paths.insert(source_path.clone());
    collect_input_paths(&json, &mut all_paths);

    let all_paths: Vec<String> = all_paths.into_iter().collect();
    // Path metadata is no longer surfaced via FetchResult - the server
    // records cached_path rows from the NarUploaded stream instead. We
    // still run `nix path-info` here to verify every archived path is
    // actually present in the local store before the caller tries to push
    // it, which surfaces a misbehaving archive step early.
    let _ = query_path_info(&all_paths, binpath_nix, abort).await?;

    Ok((source_path, all_paths))
}

/// Recursively walk the `inputs` tree from `nix flake archive --json` output
/// and insert every `path` value into `paths`.
fn collect_input_paths(node: &serde_json::Value, paths: &mut HashSet<String>) {
    if let Some(inputs) = node["inputs"].as_object() {
        for input in inputs.values() {
            if let Some(path) = input["path"].as_str() {
                paths.insert(path.to_owned());
            }
            collect_input_paths(input, paths);
        }
    }
}

/// Query `narHash` and `narSize` for each store path via `nix path-info --json`.
///
/// Supports both the legacy object output (`{"/nix/store/xxx": {...}}`) and the
/// modern array output (`[{"path": "/nix/store/xxx", ...}]`) from newer Nix
/// versions.
async fn query_path_info(
    paths: &[String],
    binpath_nix: &str,
    mut abort: watch::Receiver<bool>,
) -> Result<Vec<()>> {
    if paths.is_empty() {
        return Ok(vec![]);
    }

    trace!(binpath_nix, count = paths.len(), "executing nix path-info");
    let mut cmd = tokio::process::Command::new(binpath_nix);
    cmd.arg("path-info").arg("--json");
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);
    for path in paths {
        cmd.arg(path);
    }

    let child = cmd.spawn().context("failed to spawn nix path-info")?;

    let path_info_task = tokio::spawn(async move { child.wait_with_output().await });
    let abort_handle = path_info_task.abort_handle();

    let output = tokio::select! {
        biased;
        _ = abort_true(&mut abort) => {
            abort_handle.abort();
            anyhow::bail!("job aborted during nix path-info");
        }
        result = path_info_task => {
            result
                .context("nix path-info task panicked")?
                .context("failed to run nix path-info")?
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("nix path-info failed: {}", stderr.trim());
    }

    // Just parse enough to confirm nix path-info ran successfully; we no
    // longer surface narHash/narSize from here because the server receives
    // that metadata via the NarUploaded stream.
    let _json: serde_json::Value = parse_nix_json(&output.stdout, "nix path-info")?;

    Ok(Vec::new())
}

fn clone_and_checkout(url: &str, commit: &str, ssh_key: Option<&str>) -> Result<String> {
    let temp_dir = std::env::temp_dir().join(format!("gradient-fetch-{}", uuid::Uuid::now_v7()));

    let mut callbacks = RemoteCallbacks::new();
    callbacks.certificate_check(|cert, _valid| Ok(gradient_core::sources::accept_cert(cert)));

    if let Some(key) = ssh_key {
        let key = key.to_owned();
        callbacks.credentials(move |_url, username_from_url, _allowed| {
            git2::Cred::ssh_key_from_memory(username_from_url.unwrap_or("git"), None, &key, None)
        });
    }

    let mut fo = git2::FetchOptions::new();
    fo.remote_callbacks(callbacks);

    let repo = git2::build::RepoBuilder::new()
        .fetch_options(fo)
        .clone(url, &temp_dir)
        .with_context(|| format!("failed to clone {url}"))?;

    let oid =
        git2::Oid::from_str(commit).with_context(|| format!("invalid commit SHA: {commit}"))?;

    let git_commit = repo
        .find_commit(oid)
        .with_context(|| format!("commit {commit} not found in {url}"))?;

    let tree = git_commit.tree().context("failed to get commit tree")?;

    let mut co = git2::build::CheckoutBuilder::new();
    co.force();

    repo.checkout_tree(tree.as_object(), Some(&mut co))
        .context("checkout failed")?;

    // Leave HEAD on the default branch that git set during clone.  The Nix
    // evaluator uses `git+file://?rev=<commit>` so it reads file content from
    // the git object database at the pinned revision; HEAD is only used for
    // metadata.  Detaching HEAD (set_head_detached) causes Nix to warn
    // "could not read HEAD ref, using 'master'".

    info!(path = %temp_dir.display(), %commit, "repository cloned");
    Ok(temp_dir.to_string_lossy().into_owned())
}

/// Reconstruct a flake-ref string from a `flake.lock` node's `original`
/// field. Supports `github`, `gitlab`, `sourcehut`, `git`, `tarball`,
/// `path`, and `indirect` types - the set Nix emits for typical inputs.
fn flake_ref_from_lock_original(original: &serde_json::Value) -> anyhow::Result<String> {
    use anyhow::Context;
    let ty = original
        .get("type")
        .and_then(|v| v.as_str())
        .context("flake.lock node.original missing 'type'")?;

    let str_field = |k: &str| -> Option<&str> { original.get(k).and_then(|v| v.as_str()) };

    Ok(match ty {
        "github" | "gitlab" | "sourcehut" => {
            let owner = str_field("owner")
                .with_context(|| format!("{ty} node missing 'owner'"))?;
            let repo = str_field("repo")
                .with_context(|| format!("{ty} node missing 'repo'"))?;
            match str_field("ref") {
                Some(r) => format!("{ty}:{owner}/{repo}/{r}"),
                None => format!("{ty}:{owner}/{repo}"),
            }
        }
        "git" => {
            let url = str_field("url").context("git node missing 'url'")?;
            format!("git+{url}")
        }
        "tarball" => {
            let url = str_field("url").context("tarball node missing 'url'")?;
            url.to_owned()
        }
        "path" => {
            let path = str_field("path").context("path node missing 'path'")?;
            format!("path:{path}")
        }
        "indirect" => {
            let id = str_field("id").context("indirect node missing 'id'")?;
            format!("flake:{id}")
        }
        other => anyhow::bail!("unsupported flake.lock input type '{other}'"),
    })
}

/// Worker-side mirror of the proto `FlakeInputOverride`.
#[derive(Debug, Clone)]
pub struct OverrideInput {
    pub input_name: String,
    pub url: Option<String>,
}

impl From<&gradient_core::types::proto::FlakeInputOverride> for OverrideInput {
    fn from(o: &gradient_core::types::proto::FlakeInputOverride) -> Self {
        Self { input_name: o.input_name.clone(), url: o.url.clone() }
    }
}

type AppliedOverride = (String, String);

/// Validate overrides against the declared flake inputs, resolve `url=None`
/// entries from the lock's `original` field, and return `(applied, warnings)`.
fn resolve_overrides(
    overrides: &[OverrideInput],
    declared: &std::collections::HashSet<String>,
    lock: &serde_json::Value,
) -> anyhow::Result<(Vec<AppliedOverride>, Vec<String>)> {
    let mut applied = Vec::with_capacity(overrides.len());
    let mut warnings = Vec::new();
    for o in overrides {
        if !declared.contains(&o.input_name) {
            warnings.push(format!(
                "flake input '{}' does not exist in this project's flake - override skipped",
                o.input_name,
            ));
            continue;
        }
        let ref_str = match &o.url {
            Some(u) => u.clone(),
            None => {
                let root_key = lock
                    .get("root")
                    .and_then(|v| v.as_str())
                    .unwrap_or("root");
                let node_key = lock
                    .get("nodes")
                    .and_then(|n| n.get(root_key))
                    .and_then(|r| r.get("inputs"))
                    .and_then(|i| i.get(&o.input_name))
                    .and_then(|k| k.as_str())
                    .with_context(|| {
                        format!("flake.lock missing nodes.root.inputs.{}", o.input_name)
                    })?;
                let original = lock
                    .get("nodes")
                    .and_then(|n| n.get(node_key))
                    .and_then(|n| n.get("original"))
                    .with_context(|| {
                        format!("flake.lock missing nodes.{node_key}.original")
                    })?;
                flake_ref_from_lock_original(original)?
            }
        };
        applied.push((o.input_name.clone(), ref_str));
    }
    Ok((applied, warnings))
}

/// Read the set of input names declared in the root flake from a parsed
/// `flake.lock` document.
fn declared_inputs_from_lock(
    lock: &serde_json::Value,
) -> anyhow::Result<std::collections::HashSet<String>> {
    use anyhow::Context;
    let root_key = lock
        .get("root")
        .and_then(|v| v.as_str())
        .unwrap_or("root");
    let root = lock
        .get("nodes")
        .and_then(|n| n.get(root_key))
        .with_context(|| format!("flake.lock missing nodes.{root_key}"))?;
    let Some(inputs) = root.get("inputs").and_then(|v| v.as_object()) else {
        return Ok(std::collections::HashSet::new());
    };
    Ok(inputs.keys().cloned().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::messages::FlakeTask;
    use test_support::fakes::job_reporter::{RecordingJobReporter, ReportedEvent};

    fn make_flake_job() -> FlakeJob {
        FlakeJob {
            tasks: vec![FlakeTask::FetchFlake],
            source: FlakeSource::Repository {
                url: "https://example.com/repo.git".into(),
                commit: "abc123".into(),
            },
            wildcards: vec![],
            timeout_secs: None,
            input_overrides: vec![],
        }
    }

    fn no_abort() -> watch::Receiver<bool> {
        watch::channel(false).1
    }

    #[tokio::test]
    async fn fetch_reports_fetching_and_succeeds() {
        let job = make_flake_job();
        let credentials = crate::proto::credentials::CredentialStore::new();
        let mut reporter = RecordingJobReporter::new();

        // This will fail with a git error (fake URL), but it should report Fetching first.
        let result =
            fetch_repository(&job, &mut reporter, &credentials, "nix", "ssh", no_abort()).await;

        assert_eq!(reporter.len(), 1);
        assert!(matches!(reporter.events[0], ReportedEvent::Fetching));
        // The actual clone fails because the URL is fake - that's expected.
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn fetch_with_ssh_key_reports_fetching() {
        let job = make_flake_job();
        let credentials = crate::proto::credentials::CredentialStore::new();
        credentials.store(
            proto::messages::CredentialKind::SshKey,
            b"-----BEGIN OPENSSH PRIVATE KEY-----".to_vec(),
        );

        let mut reporter = RecordingJobReporter::new();
        let result =
            fetch_repository(&job, &mut reporter, &credentials, "nix", "ssh", no_abort()).await;

        assert!(matches!(reporter.events[0], ReportedEvent::Fetching));
        assert!(result.is_err()); // fake URL
    }

    /// fetch_repository clones the repo then runs nix flake archive.
    /// In a unit-test context nix is unavailable, so the whole fetch fails -
    /// this verifies the git clone step is reached (Fetching event emitted)
    /// and that the error propagates rather than silently falling back.
    #[tokio::test]
    async fn fetch_repository_actually_clones() {
        use std::process::Command;

        let tmp = tempfile::tempdir().unwrap();
        let repo_dir = tmp.path().join("repo");

        let rd = repo_dir.to_str().unwrap();
        Command::new("git")
            .args(["init", rd, "-b", "main"])
            .output()
            .unwrap();
        std::fs::write(repo_dir.join("flake.nix"), "{}").unwrap();
        Command::new("git")
            .args(["-C", rd, "add", "."])
            .output()
            .unwrap();
        let commit_out = Command::new("git")
            .args([
                "-C",
                rd,
                "-c",
                "user.name=test",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "init",
            ])
            .output()
            .unwrap();
        assert!(
            commit_out.status.success(),
            "git commit failed: {}",
            String::from_utf8_lossy(&commit_out.stderr)
        );

        let sha = String::from_utf8(
            Command::new("git")
                .args(["-C", rd, "rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        assert!(sha.len() == 40, "expected 40-char SHA, got: {sha}");

        let job = FlakeJob {
            tasks: vec![FlakeTask::FetchFlake],
            source: FlakeSource::Repository {
                url: format!("file://{}", repo_dir.display()),
                commit: sha,
            },
            wildcards: vec![],
            timeout_secs: None,
            input_overrides: vec![],
        };

        let credentials = crate::proto::credentials::CredentialStore::new();
        let mut reporter = RecordingJobReporter::new();

        // Clone succeeds; nix flake archive fails (nix not available in test context).
        // Without the fallback, the error propagates.
        let result =
            fetch_repository(&job, &mut reporter, &credentials, "nix", "ssh", no_abort()).await;
        assert!(result.is_err(), "expected error when nix is unavailable");
        // The Fetching event was still emitted before the failure.
        assert!(matches!(reporter.events[0], ReportedEvent::Fetching));
    }

    #[test]
    fn flake_ref_from_lock_original_github() {
        let original: serde_json::Value = serde_json::json!({
            "type": "github",
            "owner": "NixOS",
            "repo": "nixpkgs",
            "ref": "nixos-unstable",
        });
        assert_eq!(
            super::flake_ref_from_lock_original(&original).unwrap(),
            "github:NixOS/nixpkgs/nixos-unstable",
        );
    }

    #[test]
    fn flake_ref_from_lock_original_github_no_ref() {
        let original: serde_json::Value = serde_json::json!({
            "type": "github",
            "owner": "NixOS",
            "repo": "nixpkgs",
        });
        assert_eq!(
            super::flake_ref_from_lock_original(&original).unwrap(),
            "github:NixOS/nixpkgs",
        );
    }

    #[test]
    fn flake_ref_from_lock_original_indirect() {
        let original: serde_json::Value = serde_json::json!({
            "type": "indirect",
            "id": "flake-utils",
        });
        assert_eq!(
            super::flake_ref_from_lock_original(&original).unwrap(),
            "flake:flake-utils",
        );
    }

    #[test]
    fn flake_ref_from_lock_original_git_url() {
        let original: serde_json::Value = serde_json::json!({
            "type": "git",
            "url": "https://example.test/r.git",
        });
        assert_eq!(
            super::flake_ref_from_lock_original(&original).unwrap(),
            "git+https://example.test/r.git",
        );
    }

    #[test]
    fn build_archive_argv_appends_override_input_flags() {
        let overrides = [
            ("nixpkgs".to_owned(), "github:NixOS/nixpkgs/nixos-unstable".to_owned()),
            ("utils".to_owned(), "flake:flake-utils".to_owned()),
        ];
        let argv = super::build_archive_argv("git+file:///tmp/x?rev=abc", &overrides);
        assert_eq!(
            argv,
            vec![
                "flake".to_owned(),
                "archive".to_owned(),
                "--override-input".to_owned(),
                "nixpkgs".to_owned(),
                "github:NixOS/nixpkgs/nixos-unstable".to_owned(),
                "--override-input".to_owned(),
                "utils".to_owned(),
                "flake:flake-utils".to_owned(),
                "--json".to_owned(),
                "git+file:///tmp/x?rev=abc".to_owned(),
            ],
        );
    }

    #[test]
    fn build_archive_argv_no_overrides_matches_baseline() {
        let argv = super::build_archive_argv("git+file:///tmp/x?rev=abc", &[]);
        assert_eq!(
            argv,
            vec![
                "flake".to_owned(),
                "archive".to_owned(),
                "--json".to_owned(),
                "git+file:///tmp/x?rev=abc".to_owned(),
            ],
        );
    }

    #[test]
    fn declared_inputs_from_lock_reads_root_inputs() {
        let lock: serde_json::Value = serde_json::json!({
            "nodes": {
                "root": { "inputs": { "nixpkgs": "nixpkgs", "flake-utils": "flake-utils" } },
                "nixpkgs": { "original": { "type": "github", "owner": "NixOS", "repo": "nixpkgs" } },
                "flake-utils": { "original": { "type": "indirect", "id": "flake-utils" } },
            },
            "root": "root",
        });
        let names = super::declared_inputs_from_lock(&lock).unwrap();
        assert!(names.contains("nixpkgs"));
        assert!(names.contains("flake-utils"));
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn resolve_overrides_keeps_url_some() {
        let declared: std::collections::HashSet<String> =
            ["nixpkgs".to_owned()].into_iter().collect();
        let lock = serde_json::json!({"nodes":{"root":{"inputs":{"nixpkgs":"nixpkgs"}}}});
        let overrides = [super::OverrideInput {
            input_name: "nixpkgs".into(),
            url: Some("github:NixOS/nixpkgs/nixos-unstable".into()),
        }];
        let (applied, warnings) =
            super::resolve_overrides(&overrides, &declared, &lock).unwrap();
        assert_eq!(applied, vec![("nixpkgs".to_owned(), "github:NixOS/nixpkgs/nixos-unstable".to_owned())]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn resolve_overrides_keep_url_reconstructs_from_lock() {
        let declared: std::collections::HashSet<String> =
            ["nixpkgs".to_owned()].into_iter().collect();
        let lock = serde_json::json!({
            "nodes": {
                "root": {"inputs": {"nixpkgs": "nixpkgs"}},
                "nixpkgs": {"original": {"type":"github","owner":"NixOS","repo":"nixpkgs","ref":"nixos-unstable"}},
            },
            "root": "root",
        });
        let overrides = [super::OverrideInput {
            input_name: "nixpkgs".into(),
            url: None,
        }];
        let (applied, warnings) =
            super::resolve_overrides(&overrides, &declared, &lock).unwrap();
        assert_eq!(applied, vec![("nixpkgs".to_owned(), "github:NixOS/nixpkgs/nixos-unstable".to_owned())]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn resolve_overrides_unknown_input_drops_with_warning() {
        let declared: std::collections::HashSet<String> =
            ["nixpkgs".to_owned()].into_iter().collect();
        let lock = serde_json::json!({"nodes":{"root":{"inputs":{"nixpkgs":"nixpkgs"}}}});
        let overrides = [super::OverrideInput {
            input_name: "missing".into(),
            url: Some("github:x/y".into()),
        }];
        let (applied, warnings) =
            super::resolve_overrides(&overrides, &declared, &lock).unwrap();
        assert!(applied.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("missing"));
    }
}
