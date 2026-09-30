/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::context::TaskGitContext;
use super::remote::accept_cert;
use super::url::git_transport_url;
use crate::SourceError;
use git2::RemoteCallbacks;
use gradient_types::input::vec_to_hex;
use tracing::{debug, instrument};

/// The tip of a ref and the metadata a new evaluation records for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadCommit {
    pub hash: Vec<u8>,
    pub message: String,
    pub author_email: Option<String>,
    pub author_name: String,
}

/// The commit `refspec` names (a branch, `HEAD` or a commit hash) with its
/// metadata, in one depth-1 fetch however large the history. The local
/// transport cannot fetch shallow, and a local repository is cheap to read
/// whole.
pub(crate) fn fetch_commit(
    url: &str,
    ssh_creds: Option<(String, String)>,
    refspec: &str,
) -> Result<HeadCommit, SourceError> {
    let git_error = |e: git2::Error| SourceError::GitCommandFailed {
        stderr: e.message().to_string(),
    };
    let temp_dir = tempfile::TempDir::new().map_err(|e| SourceError::FileRead {
        reason: e.to_string(),
    })?;
    let repo = git2::Repository::init_bare(temp_dir.path()).map_err(git_error)?;

    let mut fo = git2::FetchOptions::new();
    fo.remote_callbacks(callbacks(ssh_creds));
    if !url.starts_with("file://") && !url.starts_with('/') {
        fo.depth(1);
    }

    repo.remote_anonymous(url)
        .map_err(git_error)?
        .fetch(&[refspec], Some(&mut fo), None)
        .map_err(git_error)?;

    let commit = repo
        .revparse_single("FETCH_HEAD")
        .and_then(|o| o.peel_to_commit())
        .map_err(git_error)?;

    Ok(HeadCommit {
        hash: commit.id().as_bytes().to_vec(),
        message: commit.summary().ok().flatten().unwrap_or("").to_string(),
        author_email: commit.author().email().ok().map(|s| s.to_string()),
        author_name: commit.author().name().unwrap_or("").to_string(),
    })
}

/// The refspec of the tip of `branch`, the remote HEAD when `None`.
pub(crate) fn head_refspec(branch: Option<&str>) -> String {
    branch.map_or_else(|| "HEAD".to_owned(), |b| format!("refs/heads/{b}"))
}

fn callbacks(ssh_creds: Option<(String, String)>) -> RemoteCallbacks<'static> {
    let mut callbacks = RemoteCallbacks::new();
    callbacks.certificate_check(|cert, _valid| Ok(accept_cert(cert)));
    if let Some((private_key, public_key)) = ssh_creds {
        callbacks.credentials(move |_url, username_from_url, _allowed| {
            git2::Cred::ssh_key_from_memory(
                username_from_url.unwrap_or("git"),
                Some(&public_key),
                &private_key,
                None,
            )
        });
    }

    callbacks
}

impl TaskGitContext<'_> {
    pub(super) async fn head_commit(
        &self,
        branch: Option<&str>,
    ) -> Result<HeadCommit, SourceError> {
        self.fetch_commit(head_refspec(branch)).await
    }

    /// The metadata of `commit_hash`. The fetch is also what proves the commit
    /// is reachable in the repository.
    #[instrument(skip(self), fields(task_id = %self.task.id, task_name = %self.task.name, commit_hash = %vec_to_hex(commit_hash)))]
    pub(super) async fn commit_info(&self, commit_hash: &[u8]) -> Result<HeadCommit, SourceError> {
        debug!("Fetching commit info");
        self.fetch_commit(vec_to_hex(commit_hash)).await
    }

    async fn fetch_commit(&self, refspec: String) -> Result<HeadCommit, SourceError> {
        let url = git_transport_url(&self.task.repository).to_string();
        let ssh_creds = self.ssh_creds.clone();

        tokio::task::spawn_blocking(move || fetch_commit(&url, ssh_creds, &refspec))
            .await
            .map_err(|e| SourceError::GitExecution {
                error: e.to_string(),
            })?
    }
}
