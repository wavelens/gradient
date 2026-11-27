/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::client::{HostClient, HostError};
use gradient_types::GitHostType;
use gradient_util::http::HttpClientForUserSuppliedUrls;
use gradient_util::http_base_url::{InvalidHttpBaseUrlError, ValidatedHttpBaseUrl};
use serde_json::json;
use tracing::warn;

const WEBHOOK_DISPLAY_NAME: &str = "Gradient";

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
        warn!(
            error = %gradient_util::error_cause_chain_formatting::error_chain(&e),
            "The Git host rejected the provisioning request."
        );
        if let Some(refusal) = e.allowlist_refusal() {
            return Self::Host(refusal.to_string());
        }
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

fn token_client(
    http: HttpClientForUserSuppliedUrls,
    git_host: GitHostType,
    endpoint_url: &str,
    token: &str,
) -> Result<HostClient, ProvisionError> {
    let base: ValidatedHttpBaseUrl = endpoint_url
        .parse()
        .map_err(|e: InvalidHttpBaseUrlError| ProvisionError::Host(e.to_string()))?;
    match git_host {
        GitHostType::Gitea | GitHostType::Forgejo => Ok(HostClient::gitea(http, base, token)),
        GitHostType::GitLab => Ok(HostClient::gitlab(http, base, token)),
        GitHostType::GitHub => Err(ProvisionError::Unsupported("GitHub")),
    }
}

#[derive(serde::Deserialize)]
struct TokenAccount {
    login: Option<String>,
    username: Option<String>,
}

pub async fn token_account_login(
    http: HttpClientForUserSuppliedUrls,
    git_host: GitHostType,
    endpoint_url: &str,
    token: &str,
) -> Result<String, ProvisionError> {
    let client = token_client(http, git_host, endpoint_url, token)?;
    let account: TokenAccount = client.get_json("/user").await?;
    account
        .login
        .or(account.username)
        .ok_or_else(|| ProvisionError::Host("Git host did not return an account name".to_owned()))
}

impl ForgeAccess<'_> {
    fn client(&self, http: HttpClientForUserSuppliedUrls) -> Result<HostClient, ProvisionError> {
        token_client(http, self.git_host, self.endpoint_url, self.token)
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
    http: HttpClientForUserSuppliedUrls,
    access: &ForgeAccess<'_>,
    webhook_url: &str,
    secret: &str,
) -> Result<(), ProvisionError> {
    let client = access.client(http)?;
    let hooks_path = format!("{}/hooks", access.project_path());
    let body = webhook_body(access.git_host, webhook_url, secret);
    let page = match access.git_host {
        GitHostType::GitLab => "per_page=100",
        _ => "limit=50",
    };
    let installed: Vec<InstalledWebhook> = client.get_json(&format!("{hooks_path}?{page}")).await?;
    let mut same_url = installed
        .into_iter()
        .filter(|hook| hook.url() == Some(webhook_url));
    match same_url.next() {
        Some(existing) => {
            let path = format!("{hooks_path}/{}", existing.id);
            match access.git_host {
                GitHostType::GitLab => client.put(&path, &body).await?,
                _ => client.patch(&path, &body).await?,
            }
        }
        None => client.post(&hooks_path, &body).await?,
    }
    for copy in same_url {
        client.delete(&format!("{hooks_path}/{}", copy.id)).await?;
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct InstalledWebhook {
    id: u64,
    url: Option<String>,
    config: Option<InstalledWebhookConfig>,
}

#[derive(serde::Deserialize)]
struct InstalledWebhookConfig {
    url: Option<String>,
}

impl InstalledWebhook {
    fn url(&self) -> Option<&str> {
        self.url
            .as_deref()
            .or_else(|| self.config.as_ref()?.url.as_deref())
    }
}

fn webhook_body(git_host: GitHostType, webhook_url: &str, secret: &str) -> serde_json::Value {
    match git_host {
        GitHostType::GitLab => json!({
            "name": WEBHOOK_DISPLAY_NAME,
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
            "name": WEBHOOK_DISPLAY_NAME,
            "type": host.as_path_segment(),
            "active": true,
            "events": GITEA_EVENTS,
            "config": { "url": webhook_url, "content_type": "json", "secret": secret },
        }),
    }
}

pub async fn install_deploy_key(
    http: HttpClientForUserSuppliedUrls,
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

    fn http() -> HttpClientForUserSuppliedUrls {
        crate::client::http_client_allow_loopback()
    }

    #[tokio::test]
    async fn token_account_login_reads_gitea_login_and_gitlab_username() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user"))
            .and(header("Authorization", "token gitea-token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "login": "ci-bot", "username": "ci-bot" })),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v4/user"))
            .and(header("PRIVATE-TOKEN", "gitlab-token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "username": "lab-bot" })),
            )
            .mount(&server)
            .await;

        let gitea =
            token_account_login(http(), GitHostType::Forgejo, &server.uri(), "gitea-token").await;
        let gitlab =
            token_account_login(http(), GitHostType::GitLab, &server.uri(), "gitlab-token").await;

        assert_eq!(gitea.unwrap(), "ci-bot");
        assert_eq!(gitlab.unwrap(), "lab-bot");
    }

    #[tokio::test]
    async fn token_account_login_reports_a_rejected_token_by_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let error = token_account_login(http(), GitHostType::Gitea, &server.uri(), "stale")
            .await
            .unwrap_err();

        assert!(error.to_string().contains("401"), "{error}");
    }

    #[tokio::test]
    async fn token_account_login_to_loopback_forgejo_with_empty_allowlist_names_outbound_allowed_non_public_network_targets_in_error()
     {
        let resolving_to_loopback = HttpClientForUserSuppliedUrls::new(
            gradient_util::http_validation::UserUrlDestinationAllowlist::default()
                .with_dns_lookup_function(|_, port| {
                    Box::pin(
                        async move { Ok(vec![std::net::SocketAddr::from(([127, 0, 0, 1], port))]) },
                    )
                }),
        )
        .expect("http client");

        let error = token_account_login(
            resolving_to_loopback,
            GitHostType::Forgejo,
            "http://forgejo.internal.example",
            "tok",
        )
        .await
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("outbound.allowedNonPublicNetworkTargets"),
            "{error}"
        );
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
    async fn installing_again_updates_the_existing_forgejo_webhook_and_deletes_copies() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/acme/app/hooks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "id": 7, "config": { "url": "https://other/hook" } },
                { "id": 8, "config": { "url": "https://ci/hook" } },
                { "id": 9, "config": { "url": "https://ci/hook" } },
            ])))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/repos/acme/app/hooks/8"))
            .and(body_partial_json(
                json!({ "active": true, "config": { "url": "https://ci/hook", "secret": "new" } }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": 8 })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/api/v1/repos/acme/app/hooks/9"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(201))
            .expect(0)
            .mount(&server)
            .await;

        let access = ForgeAccess {
            git_host: GitHostType::Forgejo,
            endpoint_url: &server.uri(),
            token: "tok",
            repository: "acme/app",
        };
        install_webhook(http(), &access, "https://ci/hook", "new")
            .await
            .expect("replace webhook");
    }

    #[tokio::test]
    async fn installing_again_updates_the_existing_gitlab_webhook() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v4/projects/group%2Fapp/hooks"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!([{ "id": 3, "url": "https://ci/hook" }])),
            )
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/api/v4/projects/group%2Fapp/hooks/3"))
            .and(body_partial_json(
                json!({ "url": "https://ci/hook", "token": "new" }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": 3 })))
            .expect(1)
            .mount(&server)
            .await;

        let access = ForgeAccess {
            git_host: GitHostType::GitLab,
            endpoint_url: &server.uri(),
            token: "tok",
            repository: "group/app",
        };
        install_webhook(http(), &access, "https://ci/hook", "new")
            .await
            .expect("replace webhook");
    }

    #[tokio::test]
    async fn forgejo_webhook_carries_url_secret_and_events() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/acme/app/hooks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/acme/app/hooks"))
            .and(header("Authorization", "token tok"))
            .and(body_partial_json(json!({
                "name": "Gradient",
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
