/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::context::TaskGitContext;
use super::ssh::with_private_key;
use super::url::{git_transport_url, set_shallow_unless_local};
use crate::SourceError;
use gradient_types::SecretString;
use gradient_types::input::vec_to_hex;
use tracing::{debug, instrument};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadCommit {
    pub hash: Vec<u8>,
    pub message: String,
    pub author_email: Option<String>,
    pub author_name: String,
}

pub(crate) fn fetch_commit(
    url: &str,
    private_key: Option<&str>,
    refspec: &str,
) -> Result<HeadCommit, SourceError> {
    with_private_key(private_key, || fetch_commit_into_temp_repo(url, refspec))
}

fn fetch_commit_into_temp_repo(url: &str, refspec: &str) -> Result<HeadCommit, SourceError> {
    let git_error = |e: git2::Error| SourceError::GitCommandFailed {
        stderr: e.message().to_string(),
    };
    let temp_dir = tempfile::TempDir::new().map_err(|e| SourceError::FileRead {
        reason: e.to_string(),
    })?;
    let repo = git2::Repository::init_bare(temp_dir.path()).map_err(git_error)?;

    let mut fo = git2::FetchOptions::new();
    set_shallow_unless_local(&mut fo, url);

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

pub(crate) fn head_refspec(branch: Option<&str>) -> String {
    branch.map_or_else(|| "HEAD".to_owned(), |b| format!("refs/heads/{b}"))
}

impl TaskGitContext<'_> {
    pub(super) async fn head_commit(
        &self,
        branch: Option<&str>,
    ) -> Result<(String, HeadCommit), SourceError> {
        let head = self.remote_head(branch).await?;
        let commit = self.fetch_commit(head_refspec(branch)).await?;
        Ok((head.branch, commit))
    }

    #[instrument(skip(self), fields(task_id = %self.task.id, task_name = %self.task.name, commit_hash = %vec_to_hex(commit_hash)))]
    pub(super) async fn commit_info(&self, commit_hash: &[u8]) -> Result<HeadCommit, SourceError> {
        debug!("Fetching commit info");
        self.fetch_commit(vec_to_hex(commit_hash)).await
    }

    async fn fetch_commit(&self, refspec: String) -> Result<HeadCommit, SourceError> {
        let url = git_transport_url(&self.task.repository).to_string();
        let private_key = self.private_key.clone();

        tokio::task::spawn_blocking(move || {
            fetch_commit(
                &url,
                private_key.as_ref().map(SecretString::expose),
                &refspec,
            )
        })
        .await
        .map_err(|e| SourceError::GitExecution {
            error: e.to_string(),
        })?
    }
}
