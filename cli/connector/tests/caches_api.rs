// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use connector::Client;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

#[tokio::test]
async fn list_caches_decodes_paginated() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/caches"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
                "items": [{
                    "id": "00000000-0000-0000-0000-000000000001",
                    "name": "krauterOS",
                    "display_name": "krauterOS",
                    "description": "",
                    "active": true,
                    "priority": 50,
                    "local_priority": 10,
                    "max_storage_gb": 0,
                    "public_key": "4OAlLNfJx4uizm5zOrRrN4DwT6Af+Om7U0gvzRI8YDU=",
                    "public": false,
                    "pull_through": false,
                    "created_by": "00000000-0000-0000-0000-000000000003",
                    "created_at": "2026-05-21T08:53:21.140900",
                    "managed": true
                }],
                "total": 1, "page": 1, "per_page": 50
            }))),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let res = client.caches().list().await.unwrap();
    assert_eq!(res.items.len(), 1);
    assert_eq!(res.items[0].name, "krauterOS");
}

#[tokio::test]
async fn get_cache_stats_returns_stats() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/caches/my-cache/stats"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
                "days": [], "hours": [], "minutes": [], "weeks": [],
                "storage_days": [], "storage_hours": [], "storage_minutes": [],
                "storage_weeks": [], "total_bytes": 12345, "total_nar_bytes": 12000,
                "total_packages": 42
            }))),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let stats = client.caches().stats("my-cache").await.unwrap();
    assert_eq!(stats.total_packages, 42);
}

#[tokio::test]
async fn a_busy_chunk_upload_is_retried_after_the_servers_delay() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/caches/mycache/nars/abc/chunk"))
        .respond_with(ResponseTemplate::new(503).insert_header("Retry-After", "0"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/caches/mycache/nars/abc/chunk"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
                "received": 3
            }))),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let received = client
        .caches()
        .nar_upload_chunk("mycache", "abc", 0, bytes::Bytes::from_static(&[1, 2, 3]))
        .await
        .expect("retried past the 503");
    assert_eq!(received, 3);
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_rate_limited_finalize_is_retried_after_the_servers_delay() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/caches/mycache/nars/abc/finalize"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/caches/mycache/nars/abc/finalize"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({}))))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let info = connector::api::NarinfoPart {
        store_path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-x".into(),
        file_hash: "sha256:a".into(),
        file_size: 3,
        nar_size: 3,
        nar_hash: "sha256:b".into(),
        references: vec![],
        deriver: None,
    };
    client
        .caches()
        .nar_upload_finalize("mycache", "abc", info)
        .await
        .expect("retried past the 429");
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn list_cache_members_decodes_member_list() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/caches/mycache/members"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!([
                {"id": "u1", "name": "alice", "user_display_name": "Alice"}
            ]))),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let members = client.caches().list_members("mycache").await.unwrap();
    assert_eq!(members[0].name, "alice");
    assert_eq!(members[0].user_display_name.as_deref(), Some("Alice"));
}

#[tokio::test]
async fn invite_cache_member_posts_user_and_role_as_json() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/caches/mycache/members"))
        .and(body_json(
            serde_json::json!({"user": "bob", "role": "View"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok("invited")))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    client
        .caches()
        .invite_cache_member(
            "mycache",
            connector::api::AddMemberRequest {
                user: "bob".into(),
                role: "View".into(),
                add_as_member_immediately: None,
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn test_enable_uses_post_and_disable_uses_delete_on_cache_active() {
    let server = MockServer::start().await;
    for verb in ["POST", "DELETE"] {
        Mock::given(method(verb))
            .and(path("/api/v1/caches/mycache/active"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok("done")))
            .expect(1)
            .mount(&server)
            .await;
    }

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    client.caches().activate_cache("mycache").await.unwrap();
    client.caches().deactivate_cache("mycache").await.unwrap();
}

#[tokio::test]
async fn creating_custom_role_returns_role() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/caches/mycache/roles"))
        .and(body_json(
            serde_json::json!({"name": "pusher", "permissions": ["writeCache"]}),
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
                "id": "00000000-0000-0000-0000-000000000001",
                "name": "pusher",
                "builtin": false,
                "managed": false,
                "permissions": ["writeCache"],
                "cache": null
            }))),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let role = client
        .caches()
        .create_custom_role(
            "mycache",
            connector::api::CreateRoleRequest {
                name: connector::api::IndexName("pusher".into()),
                permissions: vec!["writeCache".into()],
            },
        )
        .await
        .unwrap();
    assert_eq!(role.permissions, ["writeCache"]);
}

#[tokio::test]
async fn test_change_team_grant_patches_cache_team_with_role() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/caches/mycache/teams/ops"))
        .and(body_json(serde_json::json!({"role": "Write"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok("updated")))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    client
        .caches()
        .update_cache_team_grant_role(
            "mycache",
            "ops",
            connector::api::PatchCacheGrantRequest {
                role: "Write".into(),
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn approve_and_deny_subscription_requests_use_post_and_delete_on_project_path() {
    let server = MockServer::start().await;
    for verb in ["POST", "DELETE"] {
        Mock::given(method(verb))
            .and(path("/api/v1/caches/mycache/subscription-requests/proj"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok("done")))
            .expect(1)
            .mount(&server)
            .await;
    }

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    client
        .caches()
        .approve_subscription_request("mycache", "proj")
        .await
        .unwrap();
    client
        .caches()
        .deny_subscription_request("mycache", "proj")
        .await
        .unwrap();
}

#[tokio::test]
async fn test_connector_upstream_cache_decodes_both_protocol_probes() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(
            "/api/v1/caches/mycache/upstream-caches/00000000-0000-0000-0000-000000000002/test",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
                "ok": false,
                "message": "HTTP/2 failed",
                "http1": {"ok": true, "status": 200, "latency_ms": 12, "error": null},
                "http2": {"ok": false, "status": null, "latency_ms": 30, "error": "reset"}
            }))),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let result = client
        .caches()
        .test_upstream_cache("mycache", "00000000-0000-0000-0000-000000000002")
        .await
        .unwrap();
    assert!(result.http1.ok);
    assert_eq!(result.http2.error.as_deref(), Some("reset"));
}

#[tokio::test]
async fn create_cache_webhook_returns_secret() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/caches/mycache/webhooks"))
        .and(body_json(serde_json::json!({
            "name": "hook",
            "url": "https://example.com/hook",
            "events": ["cache.*"]
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
                "secret": "whsec",
                "webhook": {
                    "id": "1d26900e-b063-4624-9111-d3659538d1cb",
                    "scope": "cache",
                    "name": "hook",
                    "url": "https://example.com/hook",
                    "events": ["cache.*"],
                    "active": true,
                    "created_by": "00000000-0000-0000-0000-000000000003",
                    "created_at": "2026-11-09T00:00:00Z",
                    "updated_at": "2026-11-09T00:00:00Z"
                }
            }))),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let created = client
        .webhooks()
        .create(
            &connector::webhooks::WebhookResource::Cache("mycache".into()),
            connector::api::CreateWebhookRequest {
                name: "hook".into(),
                url: "https://example.com/hook".into(),
                events: vec!["cache.*".into()],
                active: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(created.secret, "whsec");
    assert_eq!(created.webhook.name, "hook");
}
