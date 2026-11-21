/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{
    CiReport, CiReporter, CiStatus, CreatedComment, GitHostCommentPayload, PullRequestSnapshot,
    ReactionKind, ReactionTarget, author_or_bot, comment_exists,
    parse_user_git_host_base_url_and_check_allowlist,
};
use crate::client::HostClient;
use crate::pr::gitlab::project_path;
use crate::pr::{BranchCommit, PrRef};
use anyhow::Result;
use async_trait::async_trait;
use gradient_util::http::HttpClientForUserSuppliedUrls;
use serde::{Deserialize, Serialize};

const MEMBERS_PER_PAGE: usize = 100;
const MEMBER_PAGES: usize = 20;

#[derive(Debug)]
pub struct GitlabReporter {
    client: HostClient,
}

impl GitlabReporter {
    pub fn new(
        client: HttpClientForUserSuppliedUrls,
        base_url: impl Into<String>,
        token: impl Into<String>,
    ) -> Result<Self> {
        let base_url = parse_user_git_host_base_url_and_check_allowlist(&client, &base_url.into())
            .map_err(|e| anyhow::anyhow!("Rejected GitLab base_url: {}", e))?;
        Ok(Self {
            client: HostClient::gitlab(client, base_url, token),
        })
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum GitlabState {
    Pending,
    Running,
    Success,
    Failed,
}

impl From<&CiStatus> for GitlabState {
    fn from(s: &CiStatus) -> Self {
        match s {
            CiStatus::Pending | CiStatus::ActionRequired => GitlabState::Pending,
            CiStatus::Running => GitlabState::Running,
            CiStatus::Success => GitlabState::Success,
            CiStatus::Failure => GitlabState::Failed,
            CiStatus::Error => GitlabState::Failed,
        }
    }
}

#[derive(Debug, Serialize)]
struct GitlabStatusPayload<'a> {
    state: GitlabState,
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_url: Option<&'a str>,
}

#[derive(Serialize)]
struct NameReactionPayload<'a> {
    name: &'a str,
}

#[derive(Deserialize)]
struct Member {
    username: String,
    access_level: i32,
}

#[derive(Deserialize)]
struct MrResponse {
    sha: String,
    source_branch: String,
    #[serde(default)]
    source_project_id: Option<u64>,
    #[serde(default)]
    target_project_id: Option<u64>,
    #[serde(default)]
    title: Option<String>,
}

impl From<MrResponse> for PullRequestSnapshot {
    fn from(mr: MrResponse) -> Self {
        let is_fork = match (mr.source_project_id, mr.target_project_id) {
            (Some(s), Some(t)) => Some(s != t),
            _ => None,
        };
        Self {
            head_sha: mr.sha,
            head_branch: mr.source_branch,
            head_clone_url: None,
            is_fork,
            title: mr.title,
        }
    }
}

fn gitlab_comment_path(owner: &str, repo: &str, pr_number: u64) -> String {
    format!(
        "{}/merge_requests/{}/notes",
        project_path(owner, repo),
        pr_number
    )
}

#[async_trait]
impl CiReporter for GitlabReporter {
    async fn report(&self, report: &CiReport) -> Result<Option<i64>> {
        let payload = GitlabStatusPayload {
            state: GitlabState::from(&report.status),
            name: &report.context,
            description: report.description.as_deref(),
            target_url: report.details_url.as_deref(),
        };
        self.client
            .post(
                &format!(
                    "{}/statuses/{}",
                    project_path(&report.owner, &report.repo),
                    report.sha
                ),
                &payload,
            )
            .await?;

        Ok(None)
    }

    async fn is_repo_writer(&self, owner: &str, repo: &str, username: &str) -> Result<bool> {
        let encoded_username: String =
            url::form_urlencoded::byte_serialize(username.as_bytes()).collect();
        for page in 1..=MEMBER_PAGES {
            let members: Vec<Member> = self
                .client
                .get_json(&format!(
                    "{}/members/all?query={encoded_username}&per_page={MEMBERS_PER_PAGE}&page={page}",
                    project_path(owner, repo),
                ))
                .await?;
            if members
                .iter()
                .any(|m| m.username.eq_ignore_ascii_case(username) && m.access_level >= 30)
            {
                return Ok(true);
            }
            if members.len() < MEMBERS_PER_PAGE {
                break;
            }
        }

        Ok(false)
    }

    async fn post_pr_comment(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        body: &str,
    ) -> Result<Option<i64>> {
        let created: CreatedComment = self
            .client
            .post_json(
                &gitlab_comment_path(owner, repo, pr_number),
                &GitHostCommentPayload { body },
            )
            .await?;

        Ok(Some(created.id))
    }

    async fn edit_pr_comment(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        comment_id: i64,
        body: &str,
    ) -> Result<bool> {
        comment_exists(
            self.client
                .put(
                    &format!(
                        "{}/{comment_id}",
                        gitlab_comment_path(owner, repo, pr_number)
                    ),
                    &GitHostCommentPayload { body },
                )
                .await,
        )
    }

    async fn add_reaction(&self, target: &ReactionTarget, kind: ReactionKind) -> Result<()> {
        let posted = self
            .client
            .post(
                &format!(
                    "{}/merge_requests/{}/notes/{}/award_emoji",
                    project_path(&target.owner, &target.repo),
                    target.pr_number,
                    target.comment_id
                ),
                &NameReactionPayload {
                    name: kind.gitlab_name(),
                },
            )
            .await;
        match posted {
            Err(e) if e.status() == Some(reqwest::StatusCode::CONFLICT) => Ok(()),
            other => Ok(other?),
        }
    }

    async fn get_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Option<PullRequestSnapshot>> {
        let mr: Option<MrResponse> = self
            .client
            .get_json_optional(&format!(
                "{}/merge_requests/{}",
                project_path(owner, repo),
                pr_number
            ))
            .await?;

        Ok(mr.map(PullRequestSnapshot::from))
    }

    async fn upsert_branch(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
        base: &str,
        commit: &BranchCommit,
    ) -> Result<String> {
        let mut commit = commit.clone();
        if commit.author.is_none() {
            let resolved = crate::pr::gitlab::authenticated_user(&self.client).await;
            commit.author = Some(author_or_bot(resolved, self.client.base_url()));
        }
        crate::git_push::force_push_lock_commit(
            format!("{}/{}/{}.git", self.client.base_url(), owner, repo),
            "oauth2".to_owned(),
            self.client.token().to_owned(),
            branch.to_owned(),
            base.to_owned(),
            commit,
        )
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
        crate::pr::gitlab::open_or_update_pr(&self.client, owner, repo, head, base, title, body)
            .await
    }

    async fn default_branch(&self, owner: &str, repo: &str) -> Result<String> {
        crate::pr::gitlab::default_branch(&self.client, owner, repo).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reporter::test_client;

    #[test]
    fn gitlab_reporter_rejects_aws_metadata_ip() {
        let err = GitlabReporter::new(test_client(), "http://169.254.169.254/", "tok").unwrap_err();
        assert!(format!("{err}").contains("Rejected GitLab base_url"));
    }

    #[test]
    fn gitlab_reporter_rejects_localhost_hostname() {
        assert!(GitlabReporter::new(test_client(), "http://localhost/", "tok").is_err());
    }

    #[test]
    fn gitlab_reporter_rejects_non_http_scheme() {
        assert!(GitlabReporter::new(test_client(), "file:///etc/passwd", "tok").is_err());
    }

    #[test]
    fn gitlab_reporter_trims_trailing_slash() {
        let r = GitlabReporter::new(test_client(), "https://gitlab.example.com/", "tok").unwrap();
        assert_eq!(r.client.base_url(), "https://gitlab.example.com");
    }

    #[test]
    fn gitlab_comment_path_url_encodes_owner_repo() {
        let path = gitlab_comment_path("group/subgroup", "demo", 7);
        assert_eq!(
            path,
            "/projects/group%2Fsubgroup%2Fdemo/merge_requests/7/notes"
        );
    }

    fn is_fork_of_merge_request_with_optional_source_and_target_project_ids(
        source_project_id: Option<u64>,
        target_project_id: Option<u64>,
    ) -> Option<bool> {
        PullRequestSnapshot::from(MrResponse {
            sha: "a".into(),
            source_branch: "patch-1".into(),
            source_project_id,
            target_project_id,
            title: None,
        })
        .is_fork
    }

    #[test]
    fn merge_request_without_project_id_has_unknown_is_fork() {
        assert_eq!(
            is_fork_of_merge_request_with_optional_source_and_target_project_ids(None, Some(1)),
            None
        );
        assert_eq!(
            is_fork_of_merge_request_with_optional_source_and_target_project_ids(Some(2), None),
            None
        );
        assert_eq!(
            is_fork_of_merge_request_with_optional_source_and_target_project_ids(None, None),
            None
        );
        assert_eq!(
            is_fork_of_merge_request_with_optional_source_and_target_project_ids(Some(1), Some(1)),
            Some(false)
        );
        assert_eq!(
            is_fork_of_merge_request_with_optional_source_and_target_project_ids(Some(2), Some(1)),
            Some(true)
        );
    }
}
