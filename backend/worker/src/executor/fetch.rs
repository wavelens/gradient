/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Fetch task — clone the repository to a local working directory.
//!
//! TODO(1.2): implement via git2 (same approach as core/src/sources/git.rs,
//! but running inside the worker rather than the server).

use anyhow::Result;
use proto::messages::FlakeJob;
use tracing::debug;

use crate::job::JobUpdater;

/// Clone (or update) the repository referenced by `job` at the specified commit.
pub async fn fetch_repository(job: &FlakeJob, updater: &mut JobUpdater<'_>) -> Result<()> {
    updater.report_fetching().await?;
    debug!(url = %job.repository, commit = %job.commit, "fetch_repository — stub");
    // TODO(1.2): clone repo, checkout job.commit_sha, return working directory path.
    Ok(())
}
