/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Multiple callers read the owner and repository from event payloads or stored JSON. All calls undergo a check against the one repository that the connection got a grant for.

use anyhow::{Result, bail};
use async_trait::async_trait;
use gradient_db::git_host_connection_repository_grants::{
    GitHostConnectionRepositoryAccessProof, GitRepositoryOwnerAndName,
};
use gradient_git_host::pr::{BranchCommit, PrRef};
use gradient_git_host::reporter::{
    CiReport, CiReporter, PullRequestSnapshot, ReactionKind, ReactionTarget,
};
use std::sync::Arc;

#[derive(Debug)]
pub struct ReporterRestrictedToGrantedRepository {
    inner: Arc<dyn CiReporter>,
    repository: GitRepositoryOwnerAndName,
}

impl ReporterRestrictedToGrantedRepository {
    pub fn new(
        inner: Arc<dyn CiReporter>,
        allowed: &GitHostConnectionRepositoryAccessProof,
    ) -> Self {
        Self::for_repository(inner, allowed.repository().clone())
    }

    fn for_repository(inner: Arc<dyn CiReporter>, repository: GitRepositoryOwnerAndName) -> Self {
        Self { inner, repository }
    }

    fn check(&self, owner: &str, repo: &str) -> Result<()> {
        if self.repository.owner.eq_ignore_ascii_case(owner)
            && self.repository.name.eq_ignore_ascii_case(repo)
        {
            return Ok(());
        }
        bail!(
            "The connection to the git host for this task can only use {}/{} and can't use {owner}/{repo}",
            self.repository.owner,
            self.repository.name
        )
    }
}

#[async_trait]
impl CiReporter for ReporterRestrictedToGrantedRepository {
    async fn report(&self, report: &CiReport) -> Result<Option<i64>> {
        self.check(&report.owner, &report.repo)?;
        self.inner.report(report).await
    }

    async fn is_repo_writer(&self, owner: &str, repo: &str, username: &str) -> Result<bool> {
        self.check(owner, repo)?;
        self.inner.is_repo_writer(owner, repo, username).await
    }

    async fn post_pr_comment(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        body: &str,
    ) -> Result<Option<i64>> {
        self.check(owner, repo)?;
        self.inner
            .post_pr_comment(owner, repo, pr_number, body)
            .await
    }

    async fn edit_pr_comment(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        comment_id: i64,
        body: &str,
    ) -> Result<bool> {
        self.check(owner, repo)?;
        self.inner
            .edit_pr_comment(owner, repo, pr_number, comment_id, body)
            .await
    }

    async fn get_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Option<PullRequestSnapshot>> {
        self.check(owner, repo)?;
        self.inner.get_pull_request(owner, repo, pr_number).await
    }

    async fn get_newest_undismissed_approval_review_commit_id_for_user(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        user_id: i64,
        webhook_sender_login: &str,
    ) -> Result<Option<Vec<u8>>> {
        self.check(owner, repo)?;
        self.inner
            .get_newest_undismissed_approval_review_commit_id_for_user(
                owner,
                repo,
                pr_number,
                user_id,
                webhook_sender_login,
            )
            .await
    }

    async fn add_reaction(&self, target: &ReactionTarget, kind: ReactionKind) -> Result<()> {
        self.check(&target.owner, &target.repo)?;
        self.inner.add_reaction(target, kind).await
    }

    async fn upsert_branch(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
        base: &str,
        commit: &BranchCommit,
    ) -> Result<String> {
        self.check(owner, repo)?;
        self.inner
            .upsert_branch(owner, repo, branch, base, commit)
            .await
    }

    async fn open_or_update_pr(
        &self,
        owner: &str,
        repo: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<PrRef> {
        self.check(owner, repo)?;
        self.inner
            .open_or_update_pr(owner, repo, head, base, title, body)
            .await
    }

    async fn default_branch(&self, owner: &str, repo: &str) -> Result<String> {
        self.check(owner, repo)?;
        self.inner.default_branch(owner, repo).await
    }

    async fn verify(&self, owner: &str, repo: &str) -> Result<()> {
        self.check(owner, repo)?;
        self.inner.verify(owner, repo).await
    }
}

#[cfg(test)]
mod tests;
