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
//!
//! TODO(1.2): implement via git2 (same approach as core/src/sources/git.rs,
//! but running inside the worker rather than the server).

use anyhow::Result;
use proto::messages::FlakeJob;
use proto::traits::JobReporter;
use tracing::debug;

use crate::credentials::CredentialStore;

/// Clone (or update) the repository referenced by `job` at the specified commit.
///
/// `credentials` may contain an SSH private key for private repository access.
pub async fn fetch_repository(
    job: &FlakeJob,
    updater: &mut dyn JobReporter,
    credentials: &CredentialStore,
) -> Result<()> {
    updater.report_fetching().await?;

    let has_ssh_key = credentials.ssh_key().is_some();
    debug!(
        url = %job.repository,
        commit = %job.commit,
        has_ssh_key,
        "fetch_repository — stub"
    );

    // TODO(1.2): clone repo using git2:
    //   - if has_ssh_key: write key to a temp file (mode 0600), pass to
    //     git2::RemoteCallbacks::credentials as SshKey.
    //   - checkout job.commit_sha.
    //   - return the working-directory path via updater or a side-channel.

    Ok(())
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
        let credentials = crate::credentials::CredentialStore::new();
        let mut reporter = RecordingJobReporter::new();

        fetch_repository(&job, &mut reporter, &credentials)
            .await
            .unwrap();

        assert_eq!(reporter.len(), 1);
        assert!(matches!(reporter.events[0], ReportedEvent::Fetching));
    }

    #[tokio::test]
    async fn fetch_with_ssh_key_reports_fetching() {
        let job = make_flake_job();
        let credentials = crate::credentials::CredentialStore::new();
        credentials.store(
            proto::messages::CredentialKind::SshKey,
            b"-----BEGIN OPENSSH PRIVATE KEY-----".to_vec(),
        );
        let mut reporter = RecordingJobReporter::new();

        fetch_repository(&job, &mut reporter, &credentials)
            .await
            .unwrap();

        assert!(matches!(reporter.events[0], ReportedEvent::Fetching));
    }
}
