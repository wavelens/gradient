/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::config::{ConfigKey, load_config_quiet};
use clap_complete::CompletionCandidate;
use connector::Client;
use std::ffi::OsStr;
use std::future::Future;
use std::time::Duration;

const COMPLETION_TIMEOUT: Duration = Duration::from_secs(2);

pub fn complete_orgs(current: &OsStr) -> Vec<CompletionCandidate> {
    run(current, |client, prefix| async move {
        org_names(&client, &prefix).await
    })
}

pub fn complete_projects(current: &OsStr) -> Vec<CompletionCandidate> {
    run(current, |client, prefix| async move {
        let Some(org) = selected_org() else {
            return Vec::new();
        };
        project_names(&client, &org, &prefix).await
    })
}

pub fn complete_workers(current: &OsStr) -> Vec<CompletionCandidate> {
    run(current, |client, prefix| async move {
        let Some(org) = selected_org() else {
            return Vec::new();
        };
        worker_ids(&client, &org, &prefix).await
    })
}

pub fn complete_caches(current: &OsStr) -> Vec<CompletionCandidate> {
    run(current, |client, prefix| async move {
        cache_names(&client, &prefix).await
    })
}

pub fn complete_subscribed_caches(current: &OsStr) -> Vec<CompletionCandidate> {
    run(current, |client, prefix| async move {
        let Some(org) = selected_org() else {
            return Vec::new();
        };
        subscribed_cache_names(&client, &org, &prefix).await
    })
}

pub fn complete_org_users(current: &OsStr) -> Vec<CompletionCandidate> {
    run(current, |client, prefix| async move {
        let Some(org) = selected_org() else {
            return Vec::new();
        };
        org_user_names(&client, &org, &prefix).await
    })
}

async fn org_names(client: &Client, prefix: &str) -> Vec<String> {
    match client.orgs().list().await {
        Ok(res) => matching(res.items.into_iter().map(|i| i.name), prefix),
        Err(_) => Vec::new(),
    }
}

async fn project_names(client: &Client, org: &str, prefix: &str) -> Vec<String> {
    match client.projects().list(org).await {
        Ok(res) => matching(res.items.into_iter().map(|i| i.name), prefix),
        Err(_) => Vec::new(),
    }
}

async fn worker_ids(client: &Client, org: &str, prefix: &str) -> Vec<String> {
    match client.workers().list(org).await {
        Ok(workers) => matching(workers.into_iter().map(|w| w.worker_id), prefix),
        Err(_) => Vec::new(),
    }
}

async fn cache_names(client: &Client, prefix: &str) -> Vec<String> {
    match client.caches().list().await {
        Ok(res) => matching(res.items.into_iter().map(|c| c.name), prefix),
        Err(_) => Vec::new(),
    }
}

async fn subscribed_cache_names(client: &Client, org: &str, prefix: &str) -> Vec<String> {
    match client.orgs().subscriptions(org).await {
        Ok(caches) => matching(caches.into_iter().map(|c| c.name), prefix),
        Err(_) => Vec::new(),
    }
}

async fn org_user_names(client: &Client, org: &str, prefix: &str) -> Vec<String> {
    match client.orgs().users(org).await {
        Ok(users) => matching(users.into_iter().map(|u| u.name), prefix),
        Err(_) => Vec::new(),
    }
}

fn matching(values: impl Iterator<Item = String>, prefix: &str) -> Vec<String> {
    values.filter(|v| v.starts_with(prefix)).collect()
}

fn selected_org() -> Option<String> {
    load_config_quiet()
        .get(&ConfigKey::SelectedOrganization)
        .and_then(|v| v.clone())
        .filter(|s| !s.is_empty())
}

fn client_quiet() -> Option<Client> {
    let cfg = load_config_quiet();
    let server = cfg
        .get(&ConfigKey::Server)
        .and_then(|v| v.clone())
        .filter(|s| !s.is_empty())?;
    let token = cfg
        .get(&ConfigKey::AuthToken)
        .and_then(|v| v.clone())
        .filter(|t| !t.is_empty());
    let mut builder = Client::builder()
        .base_url(server)
        .timeout(COMPLETION_TIMEOUT);
    if let Some(token) = token {
        builder = builder.token(token);
    }
    builder.build().ok()
}

fn run<F, Fut>(current: &OsStr, core: F) -> Vec<CompletionCandidate>
where
    F: FnOnce(Client, String) -> Fut,
    Fut: Future<Output = Vec<String>>,
{
    let Some(prefix) = current.to_str().map(str::to_owned) else {
        return Vec::new();
    };
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return Vec::new();
    };
    rt.block_on(async move {
        let Some(client) = client_quiet() else {
            return Vec::new();
        };
        tokio::time::timeout(COMPLETION_TIMEOUT, core(client, prefix))
            .await
            .unwrap_or_default()
    })
    .into_iter()
    .map(CompletionCandidate::new)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use connector::Client;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(base_url: &str) -> Client {
        Client::builder()
            .base_url(base_url)
            .token("test-token")
            .build()
            .unwrap()
    }

    async fn mount_json(server: &MockServer, endpoint: &str, body: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path(endpoint))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn cache_names_returns_names_and_respects_prefix() {
        let server = MockServer::start().await;
        mount_json(
            &server,
            "/api/v1/caches",
            serde_json::json!({
                "error": false,
                "message": {
                    "items": [
                        {"id": "1", "name": "alpha"},
                        {"id": "2", "name": "beta"},
                        {"id": "3", "name": "alfa"}
                    ],
                    "total": 3, "page": 1, "per_page": 50
                }
            }),
        )
        .await;
        let client = client(&server.uri());

        let all = cache_names(&client, "").await;
        assert_eq!(all, vec!["alpha", "beta", "alfa"]);

        let filtered = cache_names(&client, "al").await;
        assert_eq!(filtered, vec!["alpha", "alfa"]);
    }

    #[tokio::test]
    async fn org_names_reads_paginated_items() {
        let server = MockServer::start().await;
        mount_json(
            &server,
            "/api/v1/orgs",
            serde_json::json!({
                "error": false,
                "message": {
                    "items": [
                        {"id": "1", "name": "acme"},
                        {"id": "2", "name": "acorn"}
                    ],
                    "total": 2, "page": 1, "per_page": 50
                }
            }),
        )
        .await;
        let client = client(&server.uri());

        assert_eq!(org_names(&client, "ac").await, vec!["acme", "acorn"]);
        assert_eq!(org_names(&client, "aco").await, vec!["acorn"]);
    }

    #[tokio::test]
    async fn project_names_uses_selected_org() {
        let server = MockServer::start().await;
        mount_json(
            &server,
            "/api/v1/projects/acme",
            serde_json::json!({
                "error": false,
                "message": {
                    "items": [{"id": "1", "name": "web"}],
                    "total": 1, "page": 1, "per_page": 50
                }
            }),
        )
        .await;
        let client = client(&server.uri());

        assert_eq!(project_names(&client, "acme", "").await, vec!["web"]);
    }

    #[tokio::test]
    async fn worker_ids_returns_ids() {
        let server = MockServer::start().await;
        mount_json(
            &server,
            "/api/v1/orgs/acme/workers",
            serde_json::json!({
                "error": false,
                "message": [
                    {
                        "worker_id": "builder-1", "display_name": "Builder One",
                        "registered_at": "now", "active": true, "url": null,
                        "created_by": null, "enable_fetch": true, "enable_eval": true,
                        "enable_build": true, "live": null
                    }
                ]
            }),
        )
        .await;
        let client = client(&server.uri());

        assert_eq!(worker_ids(&client, "acme", "build").await, vec!["builder-1"]);
        assert!(worker_ids(&client, "acme", "zzz").await.is_empty());
    }

    #[tokio::test]
    async fn errors_yield_no_candidates() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/caches"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let client = client(&server.uri());

        assert!(cache_names(&client, "").await.is_empty());
    }
}
