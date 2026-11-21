/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{
    CiReport, CiReporter, CiStatus, ContentReactionPayload, PullRequestSnapshot, ReactionKind,
    ReactionTarget, author_or_bot, parse_user_git_host_base_url_and_check_allowlist,
};
use crate::client::HostClient;
use crate::pr::{BranchCommit, PrRef};
use crate::webhook::GiteaPullRequest;
use anyhow::Result;
use async_trait::async_trait;
use gradient_util::http::HttpClientForUserSuppliedUrls;
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub struct GiteaReporter {
    client: HostClient,
}

impl GiteaReporter {
    pub fn new(
        client: HttpClientForUserSuppliedUrls,
        base_url: impl Into<String>,
        token: impl Into<String>,
    ) -> Result<Self> {
        let base_url = parse_user_git_host_base_url_and_check_allowlist(&client, &base_url.into())
            .map_err(|e| anyhow::anyhow!("Rejected Gitea base_url: {}", e))?;
        Ok(Self {
            client: HostClient::gitea(client, base_url, token),
        })
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum GiteaState {
    Pending,
    Success,
    Error,
    Failure,
}

impl From<&CiStatus> for GiteaState {
    fn from(s: &CiStatus) -> Self {
        match s {
            CiStatus::Pending | CiStatus::Running | CiStatus::ActionRequired => GiteaState::Pending,
            CiStatus::Success => GiteaState::Success,
            CiStatus::Failure => GiteaState::Failure,
            CiStatus::Error => GiteaState::Error,
        }
    }
}

#[derive(Debug, Serialize)]
struct GiteaStatusPayload<'a> {
    state: GiteaState,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    context: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_url: Option<&'a str>,
}

#[derive(Deserialize)]
struct PermissionResponse {
    permission: String,
}

const REVIEW_LIST_PAGE_SIZE: usize = 50;
const MAX_REVIEW_LIST_PAGE_COUNT: usize = 10;

#[derive(Debug, Deserialize)]
struct PullRequestReviewAuthor {
    #[serde(default)]
    id: Option<i64>,
    #[serde(default)]
    login: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PullRequestReview {
    id: i64,
    #[serde(default)]
    user: Option<PullRequestReviewAuthor>,
    state: String,
    #[serde(default)]
    commit_id: Option<String>,
    #[serde(default)]
    dismissed: Option<bool>,
}

// Review IDs determine the order of reviews. They stay fixed at review creation and increase with each review. The precision of `submitted_at` is one second.
fn newest_user_approval_commit_id(
    reviews: &[PullRequestReview],
    user_id: i64,
    webhook_sender_login: &str,
) -> Option<Vec<u8>> {
    let newest_verdict = reviews
        .iter()
        .filter(|review| review.user.as_ref().and_then(|user| user.id) == Some(user_id))
        .filter(|review| matches!(review.state.as_str(), "APPROVED" | "REQUEST_CHANGES"))
        .max_by_key(|review| review.id)?;
    let login_matches = newest_verdict
        .user
        .as_ref()
        .and_then(|user| user.login.as_deref())
        .is_some_and(|login| login.eq_ignore_ascii_case(webhook_sender_login));
    if newest_verdict.state != "APPROVED"
        || newest_verdict.dismissed != Some(false)
        || !login_matches
    {
        return None;
    }
    let commit = hex::decode(newest_verdict.commit_id.as_deref()?).ok()?;
    (commit.len() == 20).then_some(commit)
}

impl From<GiteaPullRequest> for PullRequestSnapshot {
    fn from(pr: GiteaPullRequest) -> Self {
        Self {
            head_branch: pr.head_branch().unwrap_or_default(),
            head_clone_url: pr
                .fork_repo(None)
                .and_then(|r| r.clone_url.as_deref())
                .and_then(|url| {
                    crate::webhook::parse_allowed_fork_repository_clone_url(url, "gitea")
                }),
            is_fork: pr.is_fork(None),
            head_sha: pr.head.sha,
            title: pr.title,
        }
    }
}

#[async_trait]
impl CiReporter for GiteaReporter {
    async fn report(&self, report: &CiReport) -> Result<Option<i64>> {
        let payload = GiteaStatusPayload {
            state: GiteaState::from(&report.status),
            description: report.description.as_deref(),
            context: &report.context,
            target_url: report.details_url.as_deref(),
        };
        self.client
            .post(
                &format!(
                    "/repos/{}/{}/statuses/{}",
                    report.owner, report.repo, report.sha
                ),
                &payload,
            )
            .await?;

        Ok(None)
    }

    async fn is_repo_writer(&self, owner: &str, repo: &str, username: &str) -> Result<bool> {
        let parsed: Option<PermissionResponse> = self
            .client
            .get_json_optional(&format!(
                "/repos/{owner}/{repo}/collaborators/{username}/permission"
            ))
            .await?;

        Ok(parsed.is_some_and(|p| matches!(p.permission.as_str(), "admin" | "owner" | "write")))
    }

    async fn post_pr_comment(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        body: &str,
    ) -> Result<Option<i64>> {
        super::github::post_pr_comment(&self.client, owner, repo, pr_number, body).await
    }

    async fn edit_pr_comment(
        &self,
        owner: &str,
        repo: &str,
        _pr_number: u64,
        comment_id: i64,
        body: &str,
    ) -> Result<bool> {
        super::github::edit_pr_comment(&self.client, owner, repo, comment_id, body).await
    }

    async fn add_reaction(&self, target: &ReactionTarget, kind: ReactionKind) -> Result<()> {
        self.client
            .post(
                &format!(
                    "/repos/{}/{}/issues/comments/{}/reactions",
                    target.owner, target.repo, target.comment_id
                ),
                &ContentReactionPayload {
                    content: kind.github_content(),
                },
            )
            .await?;

        Ok(())
    }

    async fn get_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Option<PullRequestSnapshot>> {
        let pr: Option<GiteaPullRequest> = self
            .client
            .get_json_optional(&format!("/repos/{owner}/{repo}/pulls/{pr_number}"))
            .await?;

        Ok(pr.map(PullRequestSnapshot::from))
    }

    async fn get_newest_undismissed_approval_review_commit_id_for_user(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        user_id: i64,
        webhook_sender_login: &str,
    ) -> Result<Option<Vec<u8>>> {
        let mut reviews = Vec::new();
        for page in 1..=MAX_REVIEW_LIST_PAGE_COUNT {
            let Some(page_reviews): Option<Vec<PullRequestReview>> = self
                .client
                .get_json_optional(&format!(
                    "/repos/{owner}/{repo}/pulls/{pr_number}/reviews?page={page}&limit={REVIEW_LIST_PAGE_SIZE}"
                ))
                .await?
            else {
                return Ok(None);
            };
            if page_reviews.is_empty() {
                return Ok(newest_user_approval_commit_id(
                    &reviews,
                    user_id,
                    webhook_sender_login,
                ));
            }
            reviews.extend(page_reviews);
        }

        Ok(None)
    }

    async fn upsert_branch(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
        base: &str,
        commit: &BranchCommit,
    ) -> Result<String> {
        // The Gitea contents API would stack commits on a never-rebased branch. libgit2 is needing
        // an explicit signature, and the Gradient bot is the fallback without the read:user scope.
        let mut commit = commit.clone();
        if commit.author.is_none() {
            let resolved = crate::pr::gitea::authenticated_user(&self.client).await;
            commit.author = Some(author_or_bot(resolved, self.client.base_url()));
        }
        crate::git_push::force_push_lock_commit(
            format!("{}/{}/{}.git", self.client.base_url(), owner, repo),
            self.client.token().to_owned(),
            String::new(),
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
        crate::pr::gitea::open_or_update_pr(&self.client, owner, repo, head, base, title, body)
            .await
    }

    async fn default_branch(&self, owner: &str, repo: &str) -> Result<String> {
        crate::pr::gitea::default_branch(&self.client, owner, repo).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::http_client_allow_loopback;
    use crate::reporter::test_client;
    use serde_json::{Value, json};
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const COMMIT_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const COMMIT_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const REVIEWS_PATH: &str = "/api/v1/repos/acme/infra/pulls/7/reviews";

    fn build_gitea_review_json(
        id: i64,
        user_id: i64,
        state: &str,
        commit_id: Option<&str>,
        dismissed: Option<bool>,
    ) -> Value {
        let mut review = json!({
            "id": id,
            "user": { "id": user_id, "login": "maintainer" },
            "state": state,
        });
        if let Some(commit_id) = commit_id {
            review["commit_id"] = json!(commit_id);
        }
        if let Some(dismissed) = dismissed {
            review["dismissed"] = json!(dismissed);
        }
        review
    }

    fn select_approved_commit_from_reviews(reviews: Vec<Value>) -> Option<Vec<u8>> {
        let reviews: Vec<PullRequestReview> =
            serde_json::from_value(Value::Array(reviews)).unwrap();
        newest_user_approval_commit_id(&reviews, 42, "maintainer")
    }

    fn gitea_reporter_for_mock_server(server: &MockServer) -> GiteaReporter {
        GiteaReporter {
            client: HostClient::gitea(
                http_client_allow_loopback(),
                server.uri().parse().expect("base url"),
                "tok",
            ),
        }
    }

    #[test]
    fn test_reviews_do_not_approve_when_newer_request_changes_dismissed_approval_or_other_user_id_with_same_login()
     {
        let build_undismissed_approved_review_json =
            |id, commit| build_gitea_review_json(id, 42, "APPROVED", Some(commit), Some(false));
        let expected_decoded_commit_a_bytes = Some(hex::decode(COMMIT_A).unwrap());

        assert_eq!(
            select_approved_commit_from_reviews(vec![build_undismissed_approved_review_json(
                1, COMMIT_A
            )]),
            expected_decoded_commit_a_bytes
        );
        assert_eq!(
            select_approved_commit_from_reviews(vec![
                build_undismissed_approved_review_json(1, COMMIT_A),
                build_gitea_review_json(2, 42, "COMMENT", Some(COMMIT_B), Some(false)),
                build_gitea_review_json(3, 42, "PENDING", Some(COMMIT_B), Some(false)),
                build_gitea_review_json(4, 42, "REQUEST_REVIEW", Some(COMMIT_B), Some(false)),
            ]),
            expected_decoded_commit_a_bytes
        );
        assert_eq!(
            select_approved_commit_from_reviews(vec![
                build_undismissed_approved_review_json(1, COMMIT_A),
                build_gitea_review_json(2, 42, "REQUEST_CHANGES", Some(COMMIT_B), Some(false)),
            ]),
            None
        );
        assert_eq!(
            select_approved_commit_from_reviews(vec![
                build_gitea_review_json(5, 42, "REQUEST_CHANGES", Some(COMMIT_B), Some(false)),
                build_undismissed_approved_review_json(3, COMMIT_A),
            ]),
            None
        );
        assert_eq!(
            select_approved_commit_from_reviews(vec![
                build_undismissed_approved_review_json(1, COMMIT_A),
                build_gitea_review_json(2, 42, "APPROVED", Some(COMMIT_B), Some(true)),
            ]),
            None
        );
        assert_eq!(
            select_approved_commit_from_reviews(vec![build_gitea_review_json(
                1,
                42,
                "APPROVED",
                Some(COMMIT_A),
                None
            )]),
            None
        );
        for commit_id in [None, Some(""), Some("abc")] {
            assert_eq!(
                select_approved_commit_from_reviews(vec![build_gitea_review_json(
                    1,
                    42,
                    "APPROVED",
                    commit_id,
                    Some(false)
                )]),
                None,
                "{commit_id:?}"
            );
        }
        assert_eq!(
            select_approved_commit_from_reviews(vec![
                build_undismissed_approved_review_json(1, COMMIT_A),
                build_gitea_review_json(2, 99, "REQUEST_CHANGES", Some(COMMIT_B), Some(false)),
            ]),
            expected_decoded_commit_a_bytes
        );
        assert_eq!(
            select_approved_commit_from_reviews(vec![build_gitea_review_json(
                1,
                99,
                "APPROVED",
                Some(COMMIT_A),
                Some(false)
            )]),
            None
        );
    }

    #[test]
    fn newest_user_approval_commit_id_returns_none_when_review_user_login_differs_from_webhook_sender()
     {
        let build_approved_review_list_with_login = |login: &str| {
            let mut review =
                build_gitea_review_json(1, 42, "APPROVED", Some(COMMIT_A), Some(false));
            review["user"]["login"] = json!(login);
            vec![review]
        };
        assert_eq!(
            select_approved_commit_from_reviews(build_approved_review_list_with_login(
                "Maintainer"
            )),
            Some(hex::decode(COMMIT_A).unwrap())
        );
        assert_eq!(
            select_approved_commit_from_reviews(build_approved_review_list_with_login(
                "old-maintainer"
            )),
            None
        );
    }

    #[tokio::test]
    async fn reviews_spread_over_pages_find_newest_review_on_page_two_and_return_none_for_no_empty_page_or_404()
     {
        let server = MockServer::start().await;
        for (page, reviews) in [
            (
                "1",
                json!([build_gitea_review_json(
                    1,
                    42,
                    "APPROVED",
                    Some(COMMIT_A),
                    Some(false)
                )]),
            ),
            (
                "2",
                json!([build_gitea_review_json(
                    2,
                    42,
                    "APPROVED",
                    Some(COMMIT_B),
                    Some(false)
                )]),
            ),
            ("3", json!([])),
        ] {
            Mock::given(method("GET"))
                .and(path(REVIEWS_PATH))
                .and(query_param("page", page))
                .and(query_param("limit", "50"))
                .respond_with(ResponseTemplate::new(200).set_body_json(reviews))
                .expect(1)
                .mount(&server)
                .await;
        }
        let commit = gitea_reporter_for_mock_server(&server)
            .get_newest_undismissed_approval_review_commit_id_for_user(
                "acme",
                "infra",
                7,
                42,
                "maintainer",
            )
            .await
            .unwrap();
        assert_eq!(commit, Some(hex::decode(COMMIT_B).unwrap()));

        let mock_server_with_non_empty_reviews_on_every_page = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(REVIEWS_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                build_gitea_review_json(1, 42, "APPROVED", Some(COMMIT_A), Some(false))
            ])))
            .expect(MAX_REVIEW_LIST_PAGE_COUNT as u64)
            .mount(&mock_server_with_non_empty_reviews_on_every_page)
            .await;
        let commit =
            gitea_reporter_for_mock_server(&mock_server_with_non_empty_reviews_on_every_page)
                .get_newest_undismissed_approval_review_commit_id_for_user(
                    "acme",
                    "infra",
                    7,
                    42,
                    "maintainer",
                )
                .await
                .unwrap();
        assert_eq!(commit, None, "no empty page within the bound");

        let mock_server_returning_reviews_not_found = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(REVIEWS_PATH))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server_returning_reviews_not_found)
            .await;
        let commit = gitea_reporter_for_mock_server(&mock_server_returning_reviews_not_found)
            .get_newest_undismissed_approval_review_commit_id_for_user(
                "acme",
                "infra",
                7,
                42,
                "maintainer",
            )
            .await
            .unwrap();
        assert_eq!(commit, None, "404");
    }

    #[test]
    fn gitea_reporter_rejects_internal_or_non_http_targets() {
        for url in [
            "http://localhost:3000/",
            "http://127.0.0.1/",
            "http://10.0.0.5/",
            "http://192.168.1.1/",
            "file:///etc/passwd",
            "ftp://gitea.example.com/",
        ] {
            assert!(
                GiteaReporter::new(test_client(), url, "tok").is_err(),
                "{url}"
            );
        }
        let err = GiteaReporter::new(test_client(), "http://169.254.169.254/", "tok").unwrap_err();
        assert!(format!("{err}").contains("Rejected Gitea base_url"));
    }

    #[test]
    fn agit_pull_request_snapshot_is_untrusted_and_fetched_from_the_base_repo() {
        let repo =
            r#"{ "full_name": "org/repo", "clone_url": "https://gitea.example.com/org/repo.git" }"#;
        let body = format!(
            r#"{{
                "head": {{ "sha": "abc", "ref": "refs/pull/7/head", "label": "", "repo": {repo} }},
                "base": {{ "sha": "def", "ref": "main", "label": "main", "repo": {repo} }}
            }}"#
        );
        let pr: GiteaPullRequest = serde_json::from_str(&body).unwrap();
        let snapshot = PullRequestSnapshot::from(pr);
        assert_eq!(snapshot.is_fork, None);
        assert_eq!(snapshot.head_clone_url, None);
        assert_eq!(snapshot.head_branch, "refs/pull/7/head");
    }

    #[test]
    fn gitea_fork_clone_url_on_server_disk_gives_snapshot_without_clone_url() {
        let body = r#"{
            "head": { "sha": "abc", "ref": "patch-1", "repo": { "full_name": "external/repo", "clone_url": "file:///srv/repo" } },
            "base": { "sha": "def", "ref": "main", "repo": { "full_name": "org/repo" } }
        }"#;
        let pr: GiteaPullRequest = serde_json::from_str(body).unwrap();
        let snapshot = PullRequestSnapshot::from(pr);
        assert_eq!(snapshot.is_fork, Some(true));
        assert_eq!(snapshot.head_clone_url, None);
    }

    #[test]
    fn gitea_reporter_trims_trailing_slash() {
        let r = GiteaReporter::new(test_client(), "https://gitea.example.com/", "tok").unwrap();
        assert_eq!(r.client.base_url(), "https://gitea.example.com");
    }
}
