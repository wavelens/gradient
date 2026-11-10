/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::client::{HostClient, HostError};
use gradient_types::GitHostType;
use serde_json::json;
use tracing::warn;

const GITEA_EVENTS: &[&str] = &[
    "push",
    "create",
    "pull_request",
    "pull_request_sync",
    "issue_comment",
    "pull_request_comment",
    "pull_request_review_approved",
    "release",
];

#[derive(Debug, thiserror::Error)]
pub enum ProvisionError {
    #[error("no API on {0} to install webhooks or deploy keys")]
    Unsupported(&'static str),
    #[error("{0}")]
    Host(String),
}

impl From<HostError> for ProvisionError {
    fn from(e: HostError) -> Self {
        warn!(error = %e, "The Git host rejected the provisioning request.");
        Self::Host(match e.status() {
            Some(status) => status.to_string(),
            None => "No response from the Git host".to_owned(),
        })
    }
}

pub struct ForgeAccess<'a> {
    pub git_host: GitHostType,
    pub endpoint_url: &'a str,
    pub token: &'a str,
    pub repository: &'a str,
}

impl ForgeAccess<'_> {
    fn client(&self, http: reqwest::Client) -> Result<HostClient, ProvisionError> {
        let base = self.endpoint_url.trim_end_matches('/');
        match self.git_host {
            GitHostType::Gitea | GitHostType::Forgejo => {
                Ok(HostClient::gitea(http, base, self.token))
            }
            GitHostType::GitLab => Ok(HostClient::gitlab(http, base, self.token)),
            GitHostType::GitHub => Err(ProvisionError::Unsupported("GitHub")),
        }
    }

    fn project_path(&self) -> String {
        match self.git_host {
            GitHostType::GitLab => format!("/projects/{}", self.repository.replace('/', "%2F")),
            _ => format!("/repos/{}", self.repository),
        }
    }
}

/// The `owner/name` of the repository URL, or the full path of a nested GitLab group.
pub fn repository_path(url: &str) -> Option<String> {
    let url = url.trim();
    let url = url.split('?').next().unwrap_or(url);
    let path = match url.split_once("://") {
        Some((_, rest)) => rest.split_once('/')?.1,
        None => url.split_once(':')?.1,
    };
    let path = path.trim_matches('/').trim_end_matches(".git");
    let mut segments = path.split('/');
    let well_formed =
        segments.clone().count() >= 2 && segments.all(|s| !s.is_empty() && s != "." && s != "..");
    well_formed.then(|| path.to_owned())
}

pub async fn install_webhook(
    http: reqwest::Client,
    access: &ForgeAccess<'_>,
    webhook_url: &str,
    secret: &str,
) -> Result<(), ProvisionError> {
    let client = access.client(http)?;
    let path = format!("{}/hooks", access.project_path());
    let body = match access.git_host {
        GitHostType::GitLab => json!({
            "url": webhook_url,
            "token": secret,
            "push_events": true,
            "tag_push_events": true,
            "merge_requests_events": true,
            "note_events": true,
            "releases_events": true,
            "enable_ssl_verification": true,
        }),
        host => json!({
            "type": host.as_path_segment(),
            "active": true,
            "events": GITEA_EVENTS,
            "config": { "url": webhook_url, "content_type": "json", "secret": secret },
        }),
    };
    Ok(client.post(&path, &body).await?)
}

pub async fn install_deploy_key(
    http: reqwest::Client,
    access: &ForgeAccess<'_>,
    title: &str,
    public_key: &str,
) -> Result<(), ProvisionError> {
    let client = access.client(http)?;
    let (path, body) = match access.git_host {
        GitHostType::GitLab => (
            format!("{}/deploy_keys", access.project_path()),
            json!({ "title": title, "key": public_key, "can_push": false }),
        ),
        _ => (
            format!("{}/keys", access.project_path()),
            json!({ "title": title, "key": public_key, "read_only": true }),
        ),
    };
    Ok(client.post(&path, &body).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn http() -> reqwest::Client {
        gradient_util::http::build_client().expect("build test http client")
    }

    #[test]
    fn repository_path_covers_url_shapes() {
        assert_eq!(
            repository_path("ssh://git@forgejo.example.com/acme/app.git").as_deref(),
            Some("acme/app")
        );
        assert_eq!(
            repository_path("git@gitlab.com:group/sub/app.git").as_deref(),
            Some("group/sub/app")
        );
        assert_eq!(
            repository_path("https://codeberg.org/acme/app?dir=nix").as_deref(),
            Some("acme/app")
        );
        assert_eq!(repository_path("https://example.com/app"), None);
        for traversal in [
            "http://10.0.0.5:8080/a/../../admin/x",
            "https://example.com/acme/./app",
            "https://example.com/acme//app",
        ] {
            assert_eq!(repository_path(traversal), None, "{traversal}");
        }
    }

    #[tokio::test]
    async fn forgejo_webhook_carries_url_secret_and_events() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/acme/app/hooks"))
            .and(header("Authorization", "token tok"))
            .and(body_partial_json(json!({
                "type": "forgejo",
                "config": { "url": "https://ci/hook", "secret": "s3cret", "content_type": "json" },
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 1 })))
            .expect(1)
            .mount(&server)
            .await;

        let access = ForgeAccess {
            git_host: GitHostType::Forgejo,
            endpoint_url: &server.uri(),
            token: "tok",
            repository: "acme/app",
        };
        install_webhook(http(), &access, "https://ci/hook", "s3cret")
            .await
            .expect("install webhook");
    }

    #[tokio::test]
    async fn gitlab_deploy_key_is_read_only_and_uses_the_encoded_project_path() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v4/projects/group%2Fapp/deploy_keys"))
            .and(header("PRIVATE-TOKEN", "tok"))
            .and(body_partial_json(
                json!({ "key": "ssh-ed25519 AAAA", "can_push": false }),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 1 })))
            .expect(1)
            .mount(&server)
            .await;

        let access = ForgeAccess {
            git_host: GitHostType::GitLab,
            endpoint_url: &format!("{}/", server.uri()),
            token: "tok",
            repository: "group/app",
        };
        install_deploy_key(http(), &access, "Gradient", "ssh-ed25519 AAAA")
            .await
            .expect("install deploy key");
    }

    #[tokio::test]
    async fn a_refusal_of_the_git_host_keeps_its_status_but_not_its_body_or_url() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(403).set_body_string("token lacks admin rights"))
            .mount(&server)
            .await;

        let access = ForgeAccess {
            git_host: GitHostType::Gitea,
            endpoint_url: &server.uri(),
            token: "tok",
            repository: "acme/app",
        };
        let err = install_deploy_key(http(), &access, "Gradient", "ssh-ed25519 AAAA")
            .await
            .expect_err("a 403 must fail");
        assert_eq!(err.to_string(), "403 Forbidden");
    }
}
