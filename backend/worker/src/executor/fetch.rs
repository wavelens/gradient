/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Fetch task — clone the repository, archive it into the Nix store, and
//! upload the source + all flake inputs to the Gradient cache.
//!
//! Private repositories are accessed using the SSH private key delivered by the
//! server as a [`proto::messages::ServerMessage::Credential`] with
//! [`proto::messages::CredentialKind::SshKey`].  The key is available via
//! [`CredentialStore::ssh_key`] before this step executes.

use std::collections::HashSet;

use anyhow::{Context, Result};
use git2::RemoteCallbacks;
use proto::messages::{FetchedInput, FlakeJob};
use proto::traits::JobReporter;
use tracing::{debug, info, warn};

use crate::proto::credentials::CredentialStore;

/// Clone the repository referenced by `job`, archive it and all flake inputs
/// into the Nix store, and return the nix store source path together with
/// metadata for every archived path.
///
/// The caller is responsible for pushing the NARs (via `nar::push_direct`) and
/// reporting the result to the server (via `report_fetch_result`).
///
/// If `nix flake archive` fails, falls back to the temporary git checkout path
/// with an empty input list so evaluation can still proceed (e.g. when inputs
/// are already cached from a previous run).
pub async fn fetch_repository(
    job: &FlakeJob,
    updater: &mut dyn JobReporter,
    credentials: &CredentialStore,
    binpath_nix: &str,
) -> Result<(String, Vec<FetchedInput>)> {
    updater.report_fetching().await?;

    let url = job.repository.clone();
    let commit = job.commit.clone();
    let ssh_key = credentials
        .ssh_key()
        .map(|k| String::from_utf8_lossy(k.expose()).to_string());

    debug!(%url, %commit, has_ssh_key = ssh_key.is_some(), "fetching repository");

    let tmp_path =
        tokio::task::spawn_blocking(move || clone_and_checkout(&url, &commit, ssh_key.as_deref()))
            .await
            .context("fetch task panicked")??;

    // Archive the flake source and all locked inputs into the nix store via a
    // subprocess (so fetching goes through the nix daemon with proper network
    // and store-write access).  Returns the nix store source path so the
    // evaluator can use `path:/nix/store/xxx` — a pure, content-addressed
    // reference — instead of the git checkout in /tmp.
    let flake_ref = format!("git+file://{}?rev={}", tmp_path, job.commit);
    let binpath_nix = binpath_nix.to_owned();
    match archive_flake(&flake_ref, &binpath_nix).await {
        Ok((source_path, fetched_inputs)) => {
            info!(%source_path, inputs = fetched_inputs.len(), "flake archived to nix store");
            Ok((source_path, fetched_inputs))
        }
        Err(e) => {
            warn!(error = %e, "nix flake archive failed; falling back to git checkout path");
            Ok((tmp_path, vec![]))
        }
    }
}

/// Run `nix flake archive --json` and collect all store paths (source + all
/// transitive flake inputs).  Returns the source store path and metadata for
/// every archived path obtained from `nix path-info`.
async fn archive_flake(
    flake_ref: &str,
    binpath_nix: &str,
) -> Result<(String, Vec<FetchedInput>)> {
    let output = tokio::process::Command::new(binpath_nix)
        .args(["flake", "archive", "--json", flake_ref])
        .output()
        .await
        .context("failed to spawn nix flake archive")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("nix flake archive failed: {}", stderr.trim());
    }

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("failed to parse nix flake archive JSON")?;

    let source_path = json["path"]
        .as_str()
        .context("nix flake archive JSON missing 'path' field")?
        .to_owned();

    // Collect every store path referenced by the archive output (deduplicated).
    let mut all_paths: HashSet<String> = HashSet::new();
    all_paths.insert(source_path.clone());
    collect_input_paths(&json, &mut all_paths);

    let all_paths: Vec<String> = all_paths.into_iter().collect();
    let fetched_inputs = query_path_info(&all_paths, binpath_nix).await?;

    Ok((source_path, fetched_inputs))
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
async fn query_path_info(paths: &[String], binpath_nix: &str) -> Result<Vec<FetchedInput>> {
    if paths.is_empty() {
        return Ok(vec![]);
    }

    let mut cmd = tokio::process::Command::new(binpath_nix);
    cmd.arg("path-info").arg("--json");
    for path in paths {
        cmd.arg(path);
    }

    let output = cmd
        .output()
        .await
        .context("failed to spawn nix path-info")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("nix path-info failed: {}", stderr.trim());
    }

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("failed to parse nix path-info JSON")?;

    let mut inputs = Vec::new();

    if let Some(arr) = json.as_array() {
        // Modern Nix: array of objects with a "path" key.
        for entry in arr {
            if let Some(store_path) = entry["path"].as_str() {
                inputs.push(FetchedInput {
                    store_path: store_path.to_owned(),
                    nar_hash: entry["narHash"].as_str().unwrap_or("").to_owned(),
                    nar_size: entry["narSize"].as_u64().unwrap_or(0),
                });
            }
        }
    } else if let Some(obj) = json.as_object() {
        // Legacy Nix: object keyed by store path.
        for (store_path, info) in obj {
            inputs.push(FetchedInput {
                store_path: store_path.clone(),
                nar_hash: info["narHash"].as_str().unwrap_or("").to_owned(),
                nar_size: info["narSize"].as_u64().unwrap_or(0),
            });
        }
    }

    Ok(inputs)
}

fn clone_and_checkout(url: &str, commit: &str, ssh_key: Option<&str>) -> Result<String> {
    let temp_dir = std::env::temp_dir().join(format!("gradient-fetch-{}", uuid::Uuid::new_v4()));

    let mut callbacks = RemoteCallbacks::new();
    callbacks.certificate_check(|_cert, _valid| Ok(git2::CertificateCheckStatus::CertificateOk));

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

#[cfg(test)]
mod tests {
    use super::*;
    use proto::messages::FlakeTask;
    use test_support::fakes::job_reporter::{RecordingJobReporter, ReportedEvent};

    fn make_flake_job() -> FlakeJob {
        FlakeJob {
            tasks: vec![FlakeTask::FetchFlake],
            repository: "https://example.com/repo.git".into(),
            commit: "abc123".into(),
            wildcards: vec![],
            timeout_secs: None,
        }
    }

    #[tokio::test]
    async fn fetch_reports_fetching_and_succeeds() {
        let job = make_flake_job();
        let credentials = crate::proto::credentials::CredentialStore::new();
        let mut reporter = RecordingJobReporter::new();

        // This will fail with a git error (fake URL), but it should report Fetching first.
        let result = fetch_repository(&job, &mut reporter, &credentials, "nix").await;

        assert_eq!(reporter.len(), 1);
        assert!(matches!(reporter.events[0], ReportedEvent::Fetching));
        // The actual clone fails because the URL is fake — that's expected.
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
        let result = fetch_repository(&job, &mut reporter, &credentials, "nix").await;

        assert!(matches!(reporter.events[0], ReportedEvent::Fetching));
        assert!(result.is_err()); // fake URL
    }

    /// fetch_repository must actually clone the repository.
    /// This test creates a real local git repo and verifies the clone happens.
    /// The nix store archive will fail (no nix available in unit test context),
    /// so the fallback tmp path is returned — still verifies the clone succeeds.
    #[tokio::test]
    async fn fetch_repository_actually_clones() {
        use std::process::Command;

        let tmp = tempfile::tempdir().unwrap();
        let repo_dir = tmp.path().join("repo");

        // Create a git repository with one commit
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

        let sha_output = Command::new("git")
            .args(["-C", rd, "rev-parse", "HEAD"])
            .output()
            .unwrap();

        assert!(sha_output.status.success(), "git rev-parse failed");
        let sha = String::from_utf8(sha_output.stdout)
            .unwrap()
            .trim()
            .to_string();

        assert!(sha.len() == 40, "expected 40-char SHA, got: {sha}");
        let job = FlakeJob {
            tasks: vec![FlakeTask::FetchFlake],
            repository: format!("file://{}", repo_dir.display()),
            commit: sha,
            wildcards: vec![],
            timeout_secs: None,
        };

        let credentials = crate::proto::credentials::CredentialStore::new();
        let mut reporter = RecordingJobReporter::new();

        let result = fetch_repository(&job, &mut reporter, &credentials, "nix").await;
        assert!(
            result.is_ok(),
            "fetch should clone real repo: {:?}",
            result.err()
        );
        // The result is a (path, inputs) tuple — path should be non-empty.
        let (path, _inputs) = result.unwrap();
        assert!(!path.is_empty(), "returned path should be non-empty");
    }
}
