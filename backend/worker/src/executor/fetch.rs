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
use tracing::debug;

use crate::credentials::CredentialStore;
use crate::job::JobUpdater;

/// Clone (or update) the repository referenced by `job` at the specified commit.
///
/// `credentials` may contain an SSH private key for private repository access.
pub async fn fetch_repository(
    job: &FlakeJob,
    updater: &mut JobUpdater<'_>,
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
