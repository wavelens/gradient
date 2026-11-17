/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::checkout::Origin;
use super::commit_info::head_refspec;
use super::context::{TaskGitContext, save_observed_project_ssh_host_keys};
use super::git_credentials::{
    GitOperationCredentials, run_blocking_libgit2_operation_with_git_credentials,
};
use super::url::git_transport_url;
use crate::SourceError;
use git2::{Oid, Repository};
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComparisonBaseCommit {
    CommitHashBytes(Vec<u8>),
    BranchName(String),
}

pub(crate) fn fetch_base_and_head_into_temp_repo_and_get_changed_paths(
    base_url: &str,
    base: &ComparisonBaseCommit,
    head_url: &str,
    head: &[u8],
    credentials: &GitOperationCredentials,
) -> Result<Vec<String>, SourceError> {
    run_blocking_libgit2_operation_with_git_credentials(credentials, || {
        let temp_dir = tempfile::TempDir::new().map_err(|e| SourceError::FileRead {
            reason: e.to_string(),
        })?;
        changed_paths(temp_dir.path(), base_url, base, head_url, head).map_err(|e| {
            SourceError::GitCommandFailed {
                stderr: e.message().to_string(),
            }
        })
    })
}

fn changed_paths(
    dir: &Path,
    base_url: &str,
    base: &ComparisonBaseCommit,
    head_url: &str,
    head: &[u8],
) -> Result<Vec<String>, git2::Error> {
    let repo = Repository::init_bare(dir)?;

    let base = fetch_base(&repo, base_url, base)?;
    let head = Oid::from_bytes(head)?;
    Origin::new(&repo, head_url)?.fetch_commit(head)?;

    let base_tree = repo.find_commit(base)?.tree()?;
    let head_tree = repo.find_commit(head)?.tree()?;
    let diff = repo.diff_tree_to_tree(Some(&base_tree), Some(&head_tree), None)?;
    let paths: BTreeSet<String> = diff
        .deltas()
        .flat_map(|delta| [delta.old_file().path(), delta.new_file().path()])
        .flatten()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    Ok(paths.into_iter().collect())
}

fn fetch_base(
    repo: &Repository,
    url: &str,
    base: &ComparisonBaseCommit,
) -> Result<Oid, git2::Error> {
    let mut origin = Origin::new(repo, url)?;
    match base {
        ComparisonBaseCommit::CommitHashBytes(hash) => {
            let oid = Oid::from_bytes(hash)?;
            origin.fetch_commit(oid)?;
            Ok(oid)
        }
        ComparisonBaseCommit::BranchName(branch) => {
            origin.fetch(&head_refspec(Some(branch)), true)?;
            Ok(repo.revparse_single("FETCH_HEAD")?.peel_to_commit()?.id())
        }
    }
}

impl TaskGitContext<'_> {
    pub(super) async fn changed_paths(
        &self,
        base: ComparisonBaseCommit,
        head: Vec<u8>,
        head_repository: Option<&str>,
    ) -> Result<Vec<String>, SourceError> {
        let base_url = git_transport_url(&self.task.repository).to_owned();
        let head_url =
            git_transport_url(head_repository.unwrap_or(&self.task.repository)).to_owned();
        let credentials = self.credentials.clone();

        let changed = tokio::task::spawn_blocking(move || {
            fetch_base_and_head_into_temp_repo_and_get_changed_paths(
                &base_url,
                &base,
                &head_url,
                &head,
                &credentials,
            )
        })
        .await
        .map_err(|e| SourceError::GitExecution {
            error: e.to_string(),
        });
        save_observed_project_ssh_host_keys(
            &self.ctx.worker_db,
            self.task.project,
            self.credentials
                .ssh_git_private_key_and_known_hosts
                .as_ref(),
        )
        .await;
        changed?
    }
}
