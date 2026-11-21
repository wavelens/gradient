/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::git_credentials::{
    GitOperationCredentials, run_blocking_libgit2_operation_with_git_credentials,
};
use super::git_network_limits::{
    BlockingGitFetchOperationLimits, add_fetch_received_bytes_to_operation_total,
    blocking_git_fetch_limit_exceeded_reason_before_connect,
    build_fetch_options_with_time_and_size_limits_and_optional_shallow,
};
use super::https_credentials_for_operation_host::remote_callbacks_for_url_with_thread_https_credentials;
use anyhow::{Context, Result};
use git2::{Commit, Direction, Oid, Remote, Repository, Tree};
use gradient_types::RepositoryUrl;
use gradient_util::http_base_url::{
    remove_user_name_and_password_from_urls_in_error_text,
    upstream_cache_url_for_display_without_username_password_query_and_fragment,
};
use std::path::Path;
use tempfile::TempDir;
use tracing::debug;

pub fn checkout_commit(
    url: &RepositoryUrl,
    commit: &str,
    credentials: &GitOperationCredentials,
) -> Result<TempDir> {
    let checkout = tempfile::Builder::new()
        .prefix("gradient-fetch-")
        .tempdir()
        .context("create the checkout directory")?;
    checkout_commit_into(checkout.path(), url, commit, credentials)?;
    Ok(checkout)
}

pub fn checkout_commit_into(
    dir: &Path,
    url: &RepositoryUrl,
    commit: &str,
    credentials: &GitOperationCredentials,
) -> Result<()> {
    run_blocking_libgit2_operation_with_git_credentials(
        credentials,
        BlockingGitFetchOperationLimits::ordinary_fetch_operation_limits(),
        || check_out_into(dir, url.git_connection_url(), commit),
    )
}

pub struct FetchedCommitInTemporaryBareRepository {
    repository: Repository,
    commit: Oid,
    _directory: TempDir,
}

impl FetchedCommitInTemporaryBareRepository {
    pub fn repository(&self) -> &Repository {
        &self.repository
    }

    pub fn fetched_commit_tree(&self) -> Result<Tree<'_>> {
        self.repository
            .find_commit(self.commit)
            .and_then(|commit| commit.tree())
            .context("read the tree of the fetched commit")
    }
}

pub fn fetch_commit_into_new_bare_temporary_repository(
    url: &RepositoryUrl,
    commit: &str,
    credentials: &GitOperationCredentials,
) -> Result<FetchedCommitInTemporaryBareRepository> {
    let directory = tempfile::Builder::new()
        .prefix("gradient-fetch-bare-")
        .tempdir()
        .context("create the bare repository directory")?;
    run_blocking_libgit2_operation_with_git_credentials(
        credentials,
        BlockingGitFetchOperationLimits::ordinary_fetch_operation_limits(),
        || {
            let oid = parse_commit(commit)?;
            let repository =
                Repository::init_bare(directory.path()).context("init the bare repository")?;
            fetch_commit_by_object_id_from_url_into_repository(
                &repository,
                url.git_connection_url(),
                oid,
            )?;
            Ok(FetchedCommitInTemporaryBareRepository {
                repository,
                commit: oid,
                _directory: directory,
            })
        },
    )
}

fn parse_commit(commit: &str) -> Result<Oid> {
    Oid::from_str(commit).with_context(|| format!("invalid commit SHA: {commit}"))
}

fn fetch_commit_by_object_id_from_url_into_repository<'repo>(
    repo: &'repo Repository,
    url: &str,
    oid: Oid,
) -> Result<Commit<'repo>> {
    Origin::new(repo, url)
        .with_context(|| format!("invalid repository URL {url}"))?
        .fetch_commit(oid)
        .with_context(|| format!("fetch commit {oid} from {url}"))?;
    repo.find_commit(oid)
        .with_context(|| format!("commit {oid} not in the fetch from {url}"))
}

fn check_out_into(dir: &Path, url: &str, commit: &str) -> Result<()> {
    let oid = parse_commit(commit)?;
    let repo = Repository::init(dir).context("init the checkout")?;
    let pinned = fetch_commit_by_object_id_from_url_into_repository(&repo, url, oid)?;
    let mut options = git2::build::CheckoutBuilder::new();
    options.force();
    repo.checkout_tree(pinned.as_object(), Some(&mut options))
        .context("check out the tree")?;
    repo.set_head_detached(oid)
        .context("point HEAD at the commit")?;

    Ok(())
}

fn log_debug_fetch_by_commit_hash_refused_fetch_branches_instead_without_username_and_password(
    error: &git2::Error,
    url: &str,
) {
    let remote_url_without_user_password_and_query =
        upstream_cache_url_for_display_without_username_password_query_and_fragment(url)
            .map(|shown| shown.url);
    debug!(
        error = remove_user_name_and_password_from_urls_in_error_text(&error.to_string()),
        url = remote_url_without_user_password_and_query.as_deref(),
        "fetch by commit hash refused, fetching the branches"
    );
}

pub(super) struct Origin<'a> {
    remote: Remote<'a>,
    url: &'a str,
}

impl<'a> Origin<'a> {
    pub(super) fn new(repo: &'a Repository, url: &'a str) -> Result<Self, git2::Error> {
        let remote = repo.remote_anonymous(url)?;
        Ok(Self { remote, url })
    }

    pub(super) fn fetch_commit(&mut self, oid: Oid) -> Result<(), git2::Error> {
        if let Some(tip) = self.ref_pointing_at(oid)? {
            return self.fetch(&tip, true);
        }
        let Err(e) = self.fetch(&oid.to_string(), true) else {
            return Ok(());
        };
        log_debug_fetch_by_commit_hash_refused_fetch_branches_instead_without_username_and_password(
            &e, self.url,
        );
        self.fetch("+refs/heads/*:refs/remotes/origin/*", false)
    }

    fn ref_pointing_at(&mut self, oid: Oid) -> Result<Option<String>, git2::Error> {
        return_error_if_git_operation_limits_exceeded()?;
        let callbacks = remote_callbacks_for_url_with_thread_https_credentials(self.url);
        let connection = self
            .remote
            .connect_auth(Direction::Fetch, Some(callbacks), None)?;
        Ok(connection
            .list()?
            .iter()
            .find(|head| head.oid() == oid && !head.name().ends_with("^{}"))
            .map(|head| head.name().to_owned()))
    }

    pub(super) fn fetch(&mut self, refspec: &str, shallow: bool) -> Result<(), git2::Error> {
        return_error_if_git_operation_limits_exceeded()?;
        let mut options =
            build_fetch_options_with_time_and_size_limits_and_optional_shallow(self.url, shallow);
        let fetched = self.remote.fetch(&[refspec], Some(&mut options), None);
        add_fetch_received_bytes_to_operation_total(self.remote.stats().received_bytes());
        fetched
    }
}

fn return_error_if_git_operation_limits_exceeded() -> Result<(), git2::Error> {
    match blocking_git_fetch_limit_exceeded_reason_before_connect() {
        Some(reason) => Err(git2::Error::from_str(reason)),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn refused_fetch_by_commit_hash_debug_log_hides_user_name_and_password_from_url_and_error_text()
    {
        let log_output_temp_file = tempfile::NamedTempFile::new().unwrap();
        let captured_log_format_subscriber = tracing_subscriber::fmt()
            .with_writer(Arc::new(log_output_temp_file.reopen().unwrap()))
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .finish();
        let git_error_with_user_name_and_password_in_repository_url = git2::Error::from_str(
            "failed to fetch 'https://alice:s3cret@git.example.com/o/r': 401",
        );

        tracing::subscriber::with_default(captured_log_format_subscriber, || {
            log_debug_fetch_by_commit_hash_refused_fetch_branches_instead_without_username_and_password(
                &git_error_with_user_name_and_password_in_repository_url,
                "https://alice:s3cret@git.example.com/o/r",
            )
        });

        let captured_log_output_text =
            std::fs::read_to_string(log_output_temp_file.path()).unwrap();
        assert!(
            captured_log_output_text.contains("fetch by commit hash refused")
                && !captured_log_output_text.contains("alice")
                && !captured_log_output_text.contains("s3cret"),
            "Debug log lines for a refused fetch by commit hash show a username or password.: {captured_log_output_text}"
        );
    }
}
