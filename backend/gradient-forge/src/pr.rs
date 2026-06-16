/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Pull-request creation primitives for [`crate::reporter::CiReporter`].
//!
//! Each forge gets a small module that commits a set of file edits onto a
//! reusable branch and opens or updates a single PR for it. The reporter impls
//! delegate here so the forge-specific HTTP lives in one place.
#![allow(clippy::too_many_arguments)]

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// A file to write in a branch commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitFile {
    pub path: String,
    pub contents: Vec<u8>,
}

/// A commit to upsert onto a branch: the message, bot identity, and file edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchCommit {
    pub message: String,
    pub author_name: String,
    pub author_email: String,
    pub files: Vec<CommitFile>,
}

/// A reference to an opened or updated pull/merge request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrRef {
    pub number: i64,
    pub url: Option<String>,
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

async fn send_json<T: DeserializeOwned>(req: reqwest::RequestBuilder, ctx: &str) -> Result<T> {
    let resp = req.send().await.with_context(|| ctx.to_owned())?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("{ctx}: {status}: {body}");
    }

    resp.json::<T>().await.with_context(|| format!("{ctx}: decoding response"))
}

async fn send_ok(req: reqwest::RequestBuilder, ctx: &str) -> Result<reqwest::StatusCode> {
    let resp = req.send().await.with_context(|| ctx.to_owned())?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("{ctx}: {status}: {body}");
    }

    Ok(status)
}

// ── GitHub (shared by PAT and App reporters) ────────────────────────────────

pub(crate) mod github {
    use super::*;

    struct Gh<'a> {
        client: &'a reqwest::Client,
        token: &'a str,
    }

    impl Gh<'_> {
        fn req(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
            self.client
                .request(method, url)
                .header("User-Agent", "gradient")
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28")
                .header("Authorization", format!("Bearer {}", self.token))
        }
    }

    pub async fn upsert_branch(
        client: &reqwest::Client,
        api: &str,
        token: &str,
        owner: &str,
        repo: &str,
        branch: &str,
        base: &str,
        commit: &super::BranchCommit,
    ) -> Result<String> {
        let gh = Gh { client, token };

        let base_sha: RefObject =
            send_json(gh.req(reqwest::Method::GET, &format!("{api}/repos/{owner}/{repo}/git/ref/heads/{base}")), "github get base ref")
                .await?;
        let base_commit: CommitObject =
            send_json(gh.req(reqwest::Method::GET, &format!("{api}/repos/{owner}/{repo}/git/commits/{}", base_sha.object.sha)), "github get base commit")
                .await?;

        let mut tree = Vec::with_capacity(commit.files.len());
        for file in &commit.files {
            let blob: Sha = send_json(
                gh.req(reqwest::Method::POST, &format!("{api}/repos/{owner}/{repo}/git/blobs"))
                    .json(&BlobReq { content: b64(&file.contents), encoding: "base64" }),
                "github create blob",
            )
            .await?;
            tree.push(TreeEntry { path: file.path.clone(), mode: "100644", r#type: "blob", sha: blob.sha });
        }

        let new_tree: Sha = send_json(
            gh.req(reqwest::Method::POST, &format!("{api}/repos/{owner}/{repo}/git/trees"))
                .json(&TreeReq { base_tree: base_commit.tree.sha, tree }),
            "github create tree",
        )
        .await?;

        let ident = Ident { name: &commit.author_name, email: &commit.author_email };
        let new_commit: Sha = send_json(
            gh.req(reqwest::Method::POST, &format!("{api}/repos/{owner}/{repo}/git/commits"))
                .json(&CommitReq {
                    message: &commit.message,
                    tree: new_tree.sha,
                    parents: vec![base_sha.object.sha.clone()],
                    author: ident,
                    committer: ident,
                }),
            "github create commit",
        )
        .await?;

        let patch = gh
            .req(reqwest::Method::PATCH, &format!("{api}/repos/{owner}/{repo}/git/refs/heads/{branch}"))
            .json(&UpdateRefReq { sha: &new_commit.sha, force: true })
            .send()
            .await
            .context("github update ref")?;
        if patch.status() == reqwest::StatusCode::NOT_FOUND || patch.status() == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
            send_ok(
                gh.req(reqwest::Method::POST, &format!("{api}/repos/{owner}/{repo}/git/refs"))
                    .json(&CreateRefReq { r#ref: format!("refs/heads/{branch}"), sha: &new_commit.sha }),
                "github create ref",
            )
            .await?;
        } else if !patch.status().is_success() {
            let status = patch.status();
            let body = patch.text().await.unwrap_or_default();
            bail!("github update ref: {status}: {body}");
        }

        Ok(new_commit.sha)
    }

    pub async fn open_or_update_pr(
        client: &reqwest::Client,
        api: &str,
        token: &str,
        owner: &str,
        repo: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<super::PrRef> {
        let gh = Gh { client, token };

        let existing: Vec<PullObject> = send_json(
            gh.req(reqwest::Method::GET, &format!("{api}/repos/{owner}/{repo}/pulls?state=open&head={owner}:{head}")),
            "github list pulls",
        )
        .await?;

        if let Some(pr) = existing.into_iter().next() {
            send_ok(
                gh.req(reqwest::Method::PATCH, &format!("{api}/repos/{owner}/{repo}/pulls/{}", pr.number))
                    .json(&UpdatePullReq { title, body }),
                "github update pull",
            )
            .await?;

            return Ok(super::PrRef { number: pr.number, url: pr.html_url });
        }

        let created: PullObject = send_json(
            gh.req(reqwest::Method::POST, &format!("{api}/repos/{owner}/{repo}/pulls"))
                .json(&CreatePullReq { title, head, base, body }),
            "github create pull",
        )
        .await?;

        Ok(super::PrRef { number: created.number, url: created.html_url })
    }

    pub async fn default_branch(
        client: &reqwest::Client,
        api: &str,
        token: &str,
        owner: &str,
        repo: &str,
    ) -> Result<String> {
        let gh = Gh { client, token };
        let repo_info: RepoInfo =
            send_json(gh.req(reqwest::Method::GET, &format!("{api}/repos/{owner}/{repo}")), "github get repo")
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
        author: Ident<'a>,
        committer: Ident<'a>,
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

// ── Gitea / Forgejo (contents + branches API) ───────────────────────────────

pub(crate) mod gitea {
    use super::*;

    fn auth(req: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
        req.header("Authorization", format!("token {token}")).header("Content-Type", "application/json")
    }

    pub async fn upsert_branch(
        client: &reqwest::Client,
        base_url: &str,
        token: &str,
        owner: &str,
        repo: &str,
        branch: &str,
        base: &str,
        commit: &super::BranchCommit,
    ) -> Result<String> {
        let create_branch = auth(
            client.post(format!("{base_url}/api/v1/repos/{owner}/{repo}/branches")),
            token,
        )
        .json(&NewBranch { new_branch_name: branch, old_branch_name: base })
        .send()
        .await
        .context("gitea create branch")?;
        let st = create_branch.status();
        if !st.is_success() && st != reqwest::StatusCode::CONFLICT {
            let body = create_branch.text().await.unwrap_or_default();
            bail!("gitea create branch: {st}: {body}");
        }

        let mut head = String::new();
        for file in &commit.files {
            let existing = auth(
                client.get(format!("{base_url}/api/v1/repos/{owner}/{repo}/contents/{}?ref={branch}", file.path)),
                token,
            )
            .send()
            .await
            .context("gitea get contents")?;
            let sha = if existing.status().is_success() {
                existing.json::<Contents>().await.ok().map(|c| c.sha)
            } else {
                None
            };

            let resp: ContentsResponse = send_json(
                auth(client.put(format!("{base_url}/api/v1/repos/{owner}/{repo}/contents/{}", file.path)), token)
                    .json(&PutContents {
                        content: b64(&file.contents),
                        message: &commit.message,
                        branch,
                        sha,
                        author: GiteaIdent { name: &commit.author_name, email: &commit.author_email },
                        committer: GiteaIdent { name: &commit.author_name, email: &commit.author_email },
                    }),
                "gitea put contents",
            )
            .await?;
            head = resp.commit.sha;
        }

        Ok(head)
    }

    pub async fn open_or_update_pr(
        client: &reqwest::Client,
        base_url: &str,
        token: &str,
        owner: &str,
        repo: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<super::PrRef> {
        let open: Vec<Pull> = send_json(
            auth(client.get(format!("{base_url}/api/v1/repos/{owner}/{repo}/pulls?state=open")), token),
            "gitea list pulls",
        )
        .await?;

        if let Some(pr) = open.into_iter().find(|p| p.head.as_ref().map(|h| h.r#ref.as_str()) == Some(head)) {
            send_ok(
                auth(client.patch(format!("{base_url}/api/v1/repos/{owner}/{repo}/pulls/{}", pr.number)), token)
                    .json(&EditPull { title, body }),
                "gitea update pull",
            )
            .await?;

            return Ok(super::PrRef { number: pr.number, url: pr.html_url });
        }

        let created: Pull = send_json(
            auth(client.post(format!("{base_url}/api/v1/repos/{owner}/{repo}/pulls")), token)
                .json(&CreatePull { head, base, title, body }),
            "gitea create pull",
        )
        .await?;

        Ok(super::PrRef { number: created.number, url: created.html_url })
    }

    pub async fn default_branch(
        client: &reqwest::Client,
        base_url: &str,
        token: &str,
        owner: &str,
        repo: &str,
    ) -> Result<String> {
        let info: RepoInfo = send_json(
            auth(client.get(format!("{base_url}/api/v1/repos/{owner}/{repo}")), token),
            "gitea get repo",
        )
        .await?;

        Ok(info.default_branch)
    }

    #[derive(Deserialize)]
    struct RepoInfo {
        default_branch: String,
    }
    #[derive(Serialize)]
    struct NewBranch<'a> {
        new_branch_name: &'a str,
        old_branch_name: &'a str,
    }
    #[derive(Deserialize)]
    struct Contents {
        sha: String,
    }
    #[derive(Serialize)]
    struct GiteaIdent<'a> {
        name: &'a str,
        email: &'a str,
    }
    #[derive(Serialize)]
    struct PutContents<'a> {
        content: String,
        message: &'a str,
        branch: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        sha: Option<String>,
        author: GiteaIdent<'a>,
        committer: GiteaIdent<'a>,
    }
    #[derive(Deserialize)]
    struct ContentsResponse {
        commit: CommitSha,
    }
    #[derive(Deserialize)]
    struct CommitSha {
        sha: String,
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

// ── GitLab (commits + merge-requests API) ───────────────────────────────────

pub(crate) mod gitlab {
    use super::*;

    fn project_id(owner: &str, repo: &str) -> String {
        format!("{owner}/{repo}").replace('/', "%2F")
    }

    fn auth(req: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
        req.header("PRIVATE-TOKEN", token).header("Content-Type", "application/json")
    }

    pub async fn upsert_branch(
        client: &reqwest::Client,
        base_url: &str,
        token: &str,
        owner: &str,
        repo: &str,
        branch: &str,
        base: &str,
        commit: &super::BranchCommit,
    ) -> Result<String> {
        let id = project_id(owner, repo);
        let branch_exists = auth(
            client.get(format!("{base_url}/api/v4/projects/{id}/repository/branches/{branch}")),
            token,
        )
        .send()
        .await
        .context("gitlab get branch")?
        .status()
        .is_success();

        let actions: Vec<CommitAction> = commit
            .files
            .iter()
            .map(|f| CommitAction {
                action: "update",
                file_path: f.path.clone(),
                content: String::from_utf8_lossy(&f.contents).into_owned(),
            })
            .collect();

        let created: CommitResp = send_json(
            auth(client.post(format!("{base_url}/api/v4/projects/{id}/repository/commits")), token).json(
                &CommitReq {
                    branch,
                    start_branch: if branch_exists { None } else { Some(base) },
                    commit_message: &commit.message,
                    author_name: &commit.author_name,
                    author_email: &commit.author_email,
                    actions,
                },
            ),
            "gitlab create commit",
        )
        .await?;

        Ok(created.id)
    }

    pub async fn open_or_update_pr(
        client: &reqwest::Client,
        base_url: &str,
        token: &str,
        owner: &str,
        repo: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<super::PrRef> {
        let id = project_id(owner, repo);
        let open: Vec<Mr> = send_json(
            auth(
                client.get(format!(
                    "{base_url}/api/v4/projects/{id}/merge_requests?state=opened&source_branch={head}"
                )),
                token,
            ),
            "gitlab list MRs",
        )
        .await?;

        if let Some(mr) = open.into_iter().next() {
            send_ok(
                auth(client.put(format!("{base_url}/api/v4/projects/{id}/merge_requests/{}", mr.iid)), token)
                    .json(&EditMr { title, description: body }),
                "gitlab update MR",
            )
            .await?;

            return Ok(super::PrRef { number: mr.iid, url: mr.web_url });
        }

        let created: Mr = send_json(
            auth(client.post(format!("{base_url}/api/v4/projects/{id}/merge_requests")), token).json(
                &CreateMr { source_branch: head, target_branch: base, title, description: body },
            ),
            "gitlab create MR",
        )
        .await?;

        Ok(super::PrRef { number: created.iid, url: created.web_url })
    }

    pub async fn default_branch(
        client: &reqwest::Client,
        base_url: &str,
        token: &str,
        owner: &str,
        repo: &str,
    ) -> Result<String> {
        let id = project_id(owner, repo);
        let info: ProjectInfo = send_json(
            auth(client.get(format!("{base_url}/api/v4/projects/{id}")), token),
            "gitlab get project",
        )
        .await?;

        Ok(info.default_branch)
    }

    #[derive(Deserialize)]
    struct ProjectInfo {
        default_branch: String,
    }
    #[derive(Serialize)]
    struct CommitAction {
        action: &'static str,
        file_path: String,
        content: String,
    }
    #[derive(Serialize)]
    struct CommitReq<'a> {
        branch: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        start_branch: Option<&'a str>,
        commit_message: &'a str,
        author_name: &'a str,
        author_email: &'a str,
        actions: Vec<CommitAction>,
    }
    #[derive(Deserialize)]
    struct CommitResp {
        id: String,
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
}
