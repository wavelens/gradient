/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Fetch task — clone the repository to a local working directory.
//!
//! Private repositories are accessed using the SSH private key delivered by the
//! server as a [`proto::messages::ServerMessage::Credential`] with
//! [`proto::messages::CredentialKind::SshKey`].  The key is available via
//! [`CredentialStore::ssh_key`] before this step executes.

use anyhow::{Context, Result};
use git2::RemoteCallbacks;
use proto::messages::FlakeJob;
use proto::traits::JobReporter;
use tracing::{debug, info};

use crate::proto::credentials::CredentialStore;

/// Clone (or update) the repository referenced by `job` at the specified commit.
///
/// Returns the local path to the cloned checkout so the evaluator can use it
/// instead of fetching the remote URL again (which Nix may not support for all
/// protocols, e.g. `git://`).
///
/// `credentials` may contain an SSH private key for private repository access.
pub async fn fetch_repository(
    job: &FlakeJob,
    updater: &mut dyn JobReporter,
    credentials: &CredentialStore,
) -> Result<String> {
    updater.report_fetching().await?;

    let url = job.repository.clone();
    let commit = job.commit.clone();
    let ssh_key = credentials
        .ssh_key()
        .map(|k| String::from_utf8_lossy(k.expose()).to_string());

    debug!(%url, %commit, has_ssh_key = ssh_key.is_some(), "fetching repository");

    tokio::task::spawn_blocking(move || clone_and_checkout(&url, &commit, ssh_key.as_deref()))
        .await
        .context("fetch task panicked")?
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
        let result = fetch_repository(&job, &mut reporter, &credentials).await;

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
        let result = fetch_repository(&job, &mut reporter, &credentials).await;

        assert!(matches!(reporter.events[0], ReportedEvent::Fetching));
        assert!(result.is_err()); // fake URL
    }

    /// fetch_repository must actually clone the repository.
    /// This test creates a real local git repo and verifies the clone happens.
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

        let result = fetch_repository(&job, &mut reporter, &credentials).await;
        assert!(
            result.is_ok(),
            "fetch should clone real repo: {:?}",
            result.err()
        );
    }
}
