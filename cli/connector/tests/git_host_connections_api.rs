// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use connector::Client;
use connector::api::{
    CreateGitHostConnectionRequest, DeliveryResult, GitHostConnectionOwner, IndexName,
    PatchGitHostConnectionRequest,
};
use connector::git_host_connections::GitHostConnectionSetOwner;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CONNECTION: &str = "00000000-0000-0000-0000-000000006100";
const USER: &str = "00000000-0000-0000-0000-000000006101";
const DELIVERY: &str = "00000000-0000-0000-0000-000000006102";

fn ok<T: serde::Serialize>(m: T) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(serde_json::json!({ "error": false, "message": m }))
}

fn client(server: &MockServer) -> Client {
    Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap()
}

fn connection_response(generated_secret: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "id": CONNECTION, "owner": {"type": "project", "name": "web"},
        "name": "forgejo", "display_name": "Forgejo", "git_host_type": "forgejo",
        "base_url": "https://git.example.com", "has_secret": true, "has_access_token": false,
        "allowed_ips": [], "receives_events": true, "reports_status": false,
        "created_by": USER, "created_at": "2026-11-11T00:00:00Z", "managed": false,
        "webhook_url": "https://gradient.example/api/v1/hooks/forgejo",
        "generated_secret": generated_secret
    })
}

#[tokio::test]
async fn test_list_uses_the_path_of_each_owner_and_decodes_owner() {
    let server = MockServer::start().await;
    for (prefix, owner) in [
        ("/api/v1/admin", serde_json::json!({"type": "instance"})),
        (
            "/api/v1/teams/ops",
            serde_json::json!({"type": "team", "name": "ops"}),
        ),
        (
            "/api/v1/projects/web",
            serde_json::json!({"type": "project", "name": "web"}),
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("{prefix}/git-host-connections")))
            .respond_with(ok(serde_json::json!([{
                "id": CONNECTION, "owner": owner, "name": "forgejo", "display_name": "Forgejo",
                "git_host_type": "forgejo", "receives_events": true, "reports_status": true,
                "can_manage": true
            }])))
            .expect(1)
            .mount(&server)
            .await;
    }

    let client = client(&server);
    let api = client.git_host_connections();
    let instance = api
        .list(&GitHostConnectionSetOwner::Instance)
        .await
        .unwrap();
    assert_eq!(instance[0].owner, GitHostConnectionOwner::Instance);
    let team = api
        .list(&GitHostConnectionSetOwner::Team("ops".into()))
        .await
        .unwrap();
    assert_eq!(team[0].owner, GitHostConnectionOwner::Team("ops".into()));
    let project = api
        .list(&GitHostConnectionSetOwner::Project("web".into()))
        .await
        .unwrap();
    assert_eq!(
        GitHostConnectionSetOwner::from(&project[0].owner),
        GitHostConnectionSetOwner::Project("web".into())
    );
}

#[tokio::test]
async fn test_create_puts_body_and_returns_generated_secret() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/projects/web/git-host-connections"))
        .and(body_json(serde_json::json!({
            "name": "forgejo", "git_host_type": "forgejo",
            "base_url": "https://git.example.com", "receives_events": true
        })))
        .respond_with(ok(connection_response(Some("generated"))))
        .expect(1)
        .mount(&server)
        .await;

    let created = client(&server)
        .git_host_connections()
        .create(
            &GitHostConnectionSetOwner::Project("web".into()),
            &CreateGitHostConnectionRequest {
                name: IndexName("forgejo".into()),
                git_host_type: "forgejo".into(),
                base_url: Some("https://git.example.com".into()),
                receives_events: Some(true),
                access_token: None,
                allowed_ips: None,
                display_name: None,
                installation_id: None,
                reports_status: None,
                secret: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(created.generated_secret.as_deref(), Some("generated"));
    assert!(created.webhook_url.is_some());
}

#[tokio::test]
async fn test_get_update_delete_and_deliveries_address_one_connection() {
    let server = MockServer::start().await;
    let one = format!("/api/v1/teams/ops/git-host-connections/{CONNECTION}");
    Mock::given(method("GET"))
        .and(path(one.clone()))
        .respond_with(ok(connection_response(None)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(one.clone()))
        .and(body_json(
            serde_json::json!({"reports_status": true, "access_token": ""}),
        ))
        .respond_with(ok(connection_response(None)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(one.clone()))
        .respond_with(ok(true))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{one}/deliveries")))
        .respond_with(ok(serde_json::json!([{
            "id": DELIVERY, "event": "push", "status_code": 202, "result": "queued",
            "repository": "org/repo", "tasks_scanned": 2, "queued": 1,
            "created_at": "2026-11-11T00:00:00Z"
        }])))
        .expect(1)
        .mount(&server)
        .await;

    let client = client(&server);
    let api = client.git_host_connections();
    let owner = GitHostConnectionSetOwner::Team("ops".into());
    assert_eq!(api.get(&owner, CONNECTION).await.unwrap().name, "forgejo");
    api.update(
        &owner,
        CONNECTION,
        &PatchGitHostConnectionRequest {
            reports_status: Some(true),
            access_token: Some(String::new()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(api.delete(&owner, CONNECTION).await.unwrap());
    let deliveries = api
        .list_latest_webhook_deliveries_for_git_host_connection(&owner, CONNECTION)
        .await
        .unwrap();
    assert_eq!(deliveries[0].result, DeliveryResult::Queued);
}

#[test]
fn test_owner_parses_and_prints_the_flag_syntax() {
    for (text, owner) in [
        ("instance", GitHostConnectionSetOwner::Instance),
        ("team/ops", GitHostConnectionSetOwner::Team("ops".into())),
        (
            "project/web",
            GitHostConnectionSetOwner::Project("web".into()),
        ),
    ] {
        assert_eq!(text.parse::<GitHostConnectionSetOwner>().unwrap(), owner);
        assert_eq!(owner.to_string(), text);
    }
    for invalid in ["", "team/", "project", "org/x", "instance/x"] {
        assert!(
            invalid.parse::<GitHostConnectionSetOwner>().is_err(),
            "{invalid}"
        );
    }
}
