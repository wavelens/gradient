/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::client::HostClient;
use anyhow::Result;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitFile {
    pub path: String,
    pub contents: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitIdent {
    pub name: String,
    pub email: String,
}

impl CommitIdent {
    pub fn gradient_bot(base_url: &str) -> Self {
        Self {
            name: "Gradient".to_owned(),
            email: format!("gradient@users.noreply.{}", host_of(base_url)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchCommit {
    pub message: String,
    pub author: Option<CommitIdent>,
    pub files: Vec<CommitFile>,
}

fn host_of(base_url: &str) -> &str {
    let after_scheme = base_url.split_once("://").map_or(base_url, |(_, r)| r);
    let host = after_scheme.split('/').next().unwrap_or(after_scheme);
    host.split(':').next().unwrap_or(host)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrRef {
    pub number: i64,
    pub url: Option<String>,
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(test)]
mod ident_tests {
    use super::CommitIdent;

    #[test]
    fn gradient_bot_strips_scheme_port_and_path() {
        let id = CommitIdent::gradient_bot("https://git.example.com:3000/sub");
        assert_eq!(id.email, "gradient@users.noreply.git.example.com");
    }
}

pub(crate) mod github {
    use super::*;

    pub async fn upsert_branch(
        client: &HostClient,
        owner: &str,
        repo: &str,
        branch: &str,
        base: &str,
        commit: &super::BranchCommit,
    ) -> Result<String> {
        let gh = client.with_user_agent("gradient");

        let base_sha: RefObject = gh
            .get_json(&format!("/repos/{owner}/{repo}/git/ref/heads/{base}"))
            .await?;
        let base_commit: CommitObject = gh
            .get_json(&format!(
                "/repos/{owner}/{repo}/git/commits/{}",
                base_sha.object.sha
            ))
            .await?;

        let mut tree = Vec::with_capacity(commit.files.len());
        for file in &commit.files {
            let blob: Sha = gh
                .post_json(
                    &format!("/repos/{owner}/{repo}/git/blobs"),
                    &BlobReq {
                        content: b64(&file.contents),
                        encoding: "base64",
                    },
                )
                .await?;
            tree.push(TreeEntry {
                path: file.path.clone(),
                mode: "100644",
                r#type: "blob",
                sha: blob.sha,
            });
        }

        let new_tree: Sha = gh
            .post_json(
                &format!("/repos/{owner}/{repo}/git/trees"),
                &TreeReq {
                    base_tree: base_commit.tree.sha,
                    tree,
                },
            )
            .await?;

        // An omitted author is letting GitHub attribute the commit to the App bot and sign it
        // verified.
        let ident = commit.author.as_ref().map(|a| Ident {
            name: &a.name,
            email: &a.email,
        });
        let new_commit: Sha = gh
            .post_json(
                &format!("/repos/{owner}/{repo}/git/commits"),
                &CommitReq {
                    message: &commit.message,
                    tree: new_tree.sha,
                    parents: vec![base_sha.object.sha.clone()],
                    author: ident,
                    committer: ident,
                },
            )
            .await?;

        let updated = gh
            .patch(
                &format!("/repos/{owner}/{repo}/git/refs/heads/{branch}"),
                &UpdateRefReq {
                    sha: &new_commit.sha,
                    force: true,
                },
            )
            .await;
        match updated {
            Ok(()) => {}
            Err(e)
                if matches!(
                    e.status(),
                    Some(
                        reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::UNPROCESSABLE_ENTITY
                    )
                ) =>
            {
                gh.post(
                    &format!("/repos/{owner}/{repo}/git/refs"),
                    &CreateRefReq {
                        r#ref: format!("refs/heads/{branch}"),
                        sha: &new_commit.sha,
                    },
                )
                .await?;
            }
            Err(e) => return Err(e.into()),
        }

        Ok(new_commit.sha)
    }

    pub async fn open_or_update_pr(
        client: &HostClient,
        owner: &str,
        repo: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<super::PrRef> {
        let gh = client.with_user_agent("gradient");

        let existing: Vec<PullObject> = gh
            .get_json(&format!(
                "/repos/{owner}/{repo}/pulls?state=open&head={owner}:{head}"
            ))
            .await?;

        if let Some(pr) = existing.into_iter().next() {
            gh.patch(
                &format!("/repos/{owner}/{repo}/pulls/{}", pr.number),
                &UpdatePullReq { title, body },
            )
            .await?;

            return Ok(super::PrRef {
                number: pr.number,
                url: pr.html_url,
            });
        }

        let created: PullObject = gh
            .post_json(
                &format!("/repos/{owner}/{repo}/pulls"),
                &CreatePullReq {
                    title,
                    head,
                    base,
                    body,
                },
            )
            .await?;

        Ok(super::PrRef {
            number: created.number,
            url: created.html_url,
        })
    }

    pub async fn default_branch(client: &HostClient, owner: &str, repo: &str) -> Result<String> {
        let repo_info: RepoInfo = client
            .with_user_agent("gradient")
            .get_json(&format!("/repos/{owner}/{repo}"))
            .await?;

        Ok(repo_info.default_branch)
    }

    #[derive(Deserialize)]
    struct RepoInfo {
        default_branch: String,
    }
    #[derive(Deserialize)]
    struct RefObject {
        object: Sha,
    }
    #[derive(Deserialize)]
    struct CommitObject {
        tree: Sha,
    }
    #[derive(Deserialize)]
    struct Sha {
        sha: String,
    }
    #[derive(Deserialize)]
    struct PullObject {
        number: i64,
        html_url: Option<String>,
    }
    #[derive(Serialize)]
    struct BlobReq {
        content: String,
        encoding: &'static str,
    }
    #[derive(Serialize)]
    struct TreeEntry {
        path: String,
        mode: &'static str,
        r#type: &'static str,
        sha: String,
    }
    #[derive(Serialize)]
    struct TreeReq {
        base_tree: String,
        tree: Vec<TreeEntry>,
    }
    #[derive(Serialize, Clone, Copy)]
    struct Ident<'a> {
        name: &'a str,
        email: &'a str,
    }
    #[derive(Serialize)]
    struct CommitReq<'a> {
        message: &'a str,
        tree: String,
        parents: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        author: Option<Ident<'a>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        committer: Option<Ident<'a>>,
    }
    #[derive(Serialize)]
    struct UpdateRefReq<'a> {
        sha: &'a str,
        force: bool,
    }
    #[derive(Serialize)]
    struct CreateRefReq<'a> {
        r#ref: String,
        sha: &'a str,
    }
    #[derive(Serialize)]
    struct CreatePullReq<'a> {
        title: &'a str,
        head: &'a str,
        base: &'a str,
        body: &'a str,
    }
    #[derive(Serialize)]
    struct UpdatePullReq<'a> {
        title: &'a str,
        body: &'a str,
    }
}

pub(crate) mod gitea {
    use super::*;

    pub async fn open_or_update_pr(
        client: &HostClient,
        owner: &str,
        repo: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<super::PrRef> {
        let open: Vec<Pull> = client
            .get_json(&format!("/repos/{owner}/{repo}/pulls?state=open"))
            .await?;

        if let Some(pr) = open
            .into_iter()
            .find(|p| p.head.as_ref().map(|h| h.r#ref.as_str()) == Some(head))
        {
            client
                .patch(
                    &format!("/repos/{owner}/{repo}/pulls/{}", pr.number),
                    &EditPull { title, body },
                )
                .await?;

            return Ok(super::PrRef {
                number: pr.number,
                url: pr.html_url,
            });
        }

        let created: Pull = client
            .post_json(
                &format!("/repos/{owner}/{repo}/pulls"),
                &CreatePull {
                    head,
                    base,
                    title,
                    body,
                },
            )
            .await?;

        Ok(super::PrRef {
            number: created.number,
            url: created.html_url,
        })
    }

    pub async fn default_branch(client: &HostClient, owner: &str, repo: &str) -> Result<String> {
        let info: RepoInfo = client.get_json(&format!("/repos/{owner}/{repo}")).await?;

        Ok(info.default_branch)
    }

    pub async fn authenticated_user(client: &HostClient) -> Result<super::CommitIdent> {
        let user: User = client.get_json("/user").await?;
        let name = Some(user.full_name)
            .filter(|n| !n.trim().is_empty())
            .unwrap_or(user.login.clone());
        let email = user
            .email
            .filter(|e| !e.trim().is_empty())
            .unwrap_or_else(|| {
                format!(
                    "{}@users.noreply.{}",
                    user.login,
                    super::host_of(client.base_url())
                )
            });

        Ok(super::CommitIdent { name, email })
    }

    #[derive(Deserialize)]
    struct RepoInfo {
        default_branch: String,
    }
    #[derive(Deserialize)]
    struct User {
        login: String,
        #[serde(default)]
        full_name: String,
        email: Option<String>,
    }
    #[derive(Deserialize)]
    struct Pull {
        number: i64,
        html_url: Option<String>,
        head: Option<PullHead>,
    }
    #[derive(Deserialize)]
    struct PullHead {
        r#ref: String,
    }
    #[derive(Serialize)]
    struct CreatePull<'a> {
        head: &'a str,
        base: &'a str,
        title: &'a str,
        body: &'a str,
    }
    #[derive(Serialize)]
    struct EditPull<'a> {
        title: &'a str,
        body: &'a str,
    }
}

pub(crate) mod gitlab {
    use super::*;

    pub fn project_id(owner: &str, repo: &str) -> String {
        format!("{owner}/{repo}").replace('/', "%2F")
    }

    pub fn project_path(owner: &str, repo: &str) -> String {
        format!("/projects/{}", project_id(owner, repo))
    }

    pub async fn open_or_update_pr(
        client: &HostClient,
        owner: &str,
        repo: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<super::PrRef> {
        let project = project_path(owner, repo);
        let open: Vec<Mr> = client
            .get_json(&format!(
                "{project}/merge_requests?state=opened&source_branch={head}"
            ))
            .await?;

        if let Some(mr) = open.into_iter().next() {
            client
                .put(
                    &format!("{project}/merge_requests/{}", mr.iid),
                    &EditMr {
                        title,
                        description: body,
                    },
                )
                .await?;

            return Ok(super::PrRef {
                number: mr.iid,
                url: mr.web_url,
            });
        }

        let created: Mr = client
            .post_json(
                &format!("{project}/merge_requests"),
                &CreateMr {
                    source_branch: head,
                    target_branch: base,
                    title,
                    description: body,
                },
            )
            .await?;

        Ok(super::PrRef {
            number: created.iid,
            url: created.web_url,
        })
    }

    pub async fn default_branch(client: &HostClient, owner: &str, repo: &str) -> Result<String> {
        let info: ProjectInfo = client.get_json(&project_path(owner, repo)).await?;

        Ok(info.default_branch)
    }

    pub async fn authenticated_user(client: &HostClient) -> Result<super::CommitIdent> {
        let user: User = client.get_json("/user").await?;
        let name = Some(user.name)
            .filter(|n| !n.trim().is_empty())
            .unwrap_or(user.username.clone());
        let email = user
            .email
            .filter(|e| !e.trim().is_empty())
            .unwrap_or_else(|| {
                format!(
                    "{}@users.noreply.{}",
                    user.username,
                    super::host_of(client.base_url())
                )
            });

        Ok(super::CommitIdent { name, email })
    }

    #[derive(Deserialize)]
    struct ProjectInfo {
        default_branch: String,
    }
    #[derive(Deserialize)]
    struct User {
        username: String,
        #[serde(default)]
        name: String,
        email: Option<String>,
    }
    #[derive(Deserialize)]
    struct Mr {
        iid: i64,
        web_url: Option<String>,
    }
    #[derive(Serialize)]
    struct CreateMr<'a> {
        source_branch: &'a str,
        target_branch: &'a str,
        title: &'a str,
        description: &'a str,
    }
    #[derive(Serialize)]
    struct EditMr<'a> {
        title: &'a str,
        description: &'a str,
    }

    #[cfg(test)]
    mod tests {
        use super::{project_id, project_path};

        #[test]
        fn project_path_addresses_the_projects_endpoint_by_encoded_path() {
            assert_eq!(
                project_path("group/sub", "repo"),
                "/projects/group%2Fsub%2Frepo"
            );
        }

        #[test]
        fn project_id_flat_path() {
            assert_eq!(project_id("acme", "widgets"), "acme%2Fwidgets");
        }

        #[test]
        fn project_id_nested_groups() {
            assert_eq!(project_id("group", "sub/repo"), "group%2Fsub%2Frepo");
        }
    }
}
