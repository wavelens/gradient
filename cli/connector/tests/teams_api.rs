// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use connector::Client;
use connector::api::{
    MakeTeamRequest, PatchTeamRequest, PatchWorkerRequest, RegisterWorkerRequest,
    RemoveTeamMemberRequest, RevokeInvitationRequest, TeamMemberRequest, TeamRole,
};
use serde_json::{Value, json};
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn serve(verb: &str, endpoint: &str, message: Value) -> (MockServer, Client) {
    let server = MockServer::start().await;
    Mock::given(method(verb))
        .and(path(format!("/api/v1/{endpoint}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"error": false, "message": message})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    (server, client)
}

fn worker_entry() -> Value {
    json!({
        "worker_id": "00000000-0000-0000-0000-000000000001",
        "display_name": "builder",
        "registered_at": "2026-11-01T00:00:00",
        "active": true,
        "managed": false,
        "gradient_ci": false,
        "enable_network": true,
        "enable_secrets": false,
        "enable_exec": false,
        "enable_eval": true,
        "enable_build": true,
        "connected": false
    })
}

#[tokio::test]
async fn list_teams_decodes_summaries() {
    let (_s, client) = serve(
        "GET",
        "teams",
        json!([{"name": "ops", "display_name": "Ops", "role": "admin"}]),
    )
    .await;
    let teams = client.teams().list().await.unwrap();
    assert_eq!(teams[0].name, "ops");
    assert_eq!(teams[0].role, TeamRole::Admin);
}

#[tokio::test]
async fn create_team_puts_name_and_display_name() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/teams"))
        .and(body_json(json!({"name": "ops", "display_name": "Ops"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"error": false, "message": "00000000-0000-0000-0000-000000000002"}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let id = client
        .teams()
        .create(MakeTeamRequest {
            name: "ops".to_owned().into(),
            display_name: "Ops".to_owned().into(),
        })
        .await
        .unwrap();
    assert_eq!(id, "00000000-0000-0000-0000-000000000002");
}

#[tokio::test]
async fn get_team_decodes_settings() {
    let (_s, client) = serve(
        "GET",
        "teams/ops",
        json!({
            "id": "00000000-0000-0000-0000-000000000002",
            "name": "ops",
            "display_name": "Ops",
            "managed": false,
            "role": "member",
            "new_project_users": false,
            "new_project_workers": true
        }),
    )
    .await;
    let team = client.teams().get("ops").await.unwrap();
    assert!(team.new_project_workers);
    assert_eq!(team.role, Some(TeamRole::Member));
}

#[tokio::test]
async fn update_team_sends_only_given_fields() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/teams/ops"))
        .and(body_json(json!({"display_name": "Operations"})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"error": false, "message": "updated"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    client
        .teams()
        .update(
            "ops",
            PatchTeamRequest {
                display_name: Some("Operations".to_owned().into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn delete_team() {
    let (_s, client) = serve("DELETE", "teams/ops", json!("deleted")).await;
    client.teams().delete("ops").await.unwrap();
}

#[tokio::test]
async fn list_members_decodes_items() {
    let (_s, client) = serve(
        "GET",
        "teams/ops/members",
        json!([{"user": "alice", "name": "Alice", "role": "admin", "source": "api"}]),
    )
    .await;
    let members = client.teams().list_members("ops").await.unwrap();
    assert_eq!(members[0].user, "alice");
}

#[tokio::test]
async fn add_user_posts_member_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/teams/ops/members"))
        .and(body_json(json!({"user": "bob", "role": "member"})))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"error": false, "message": "Invitation sent"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let message = client
        .teams()
        .add_user(
            "ops",
            TeamMemberRequest {
                user: "bob".into(),
                role: TeamRole::Member,
            },
        )
        .await
        .unwrap();
    assert_eq!(message, "Invitation sent");
}

#[tokio::test]
async fn update_team_member_role_patches_members() {
    let (_s, client) = serve("PATCH", "teams/ops/members", json!("updated")).await;
    client
        .teams()
        .update_team_member_role(
            "ops",
            TeamMemberRequest {
                user: "bob".into(),
                role: TeamRole::Admin,
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn remove_team_member_deletes_members() {
    let (_s, client) = serve("DELETE", "teams/ops/members", json!("removed")).await;
    client
        .teams()
        .remove_team_member("ops", RemoveTeamMemberRequest { user: "bob".into() })
        .await
        .unwrap();
}

#[tokio::test]
async fn list_pending_invitations_decodes_items() {
    let (_s, client) = serve(
        "GET",
        "teams/ops/invitations",
        json!([{
            "user": "bob",
            "name": "Bob",
            "role": "member",
            "created_at": "2026-11-01T00:00:00",
            "expires_at": "2026-11-08T00:00:00"
        }]),
    )
    .await;
    let invitations = client
        .teams()
        .list_pending_invitations("ops")
        .await
        .unwrap();
    assert_eq!(invitations[0].user, "bob");
}

#[tokio::test]
async fn withdraw_team_invitation_deletes_invitations() {
    let (_s, client) = serve("DELETE", "teams/ops/invitations", json!("revoked")).await;
    client
        .teams()
        .withdraw_team_invitation("ops", RevokeInvitationRequest { user: "bob".into() })
        .await
        .unwrap();
}

#[tokio::test]
async fn list_team_grant_requests_decodes_items() {
    let (_s, client) = serve(
        "GET",
        "teams/ops/requests",
        json!([{
            "id": "0192f0a0-0000-7000-8000-000000000003",
            "kind": "project",
            "target": "web",
            "display_name": "Web",
            "users": true,
            "workers": false,
            "created_at": "2026-11-01T00:00:00"
        }]),
    )
    .await;
    let requests = client
        .teams()
        .list_team_grant_requests("ops")
        .await
        .unwrap();
    assert_eq!(requests[0].target, "web");
}

#[tokio::test]
async fn approve_grant_request_posts_request_id() {
    let (_s, client) = serve(
        "POST",
        "teams/ops/requests/0192f0a0-0000-7000-8000-000000000003",
        json!("approved"),
    )
    .await;
    client
        .teams()
        .approve_grant_request("ops", "0192f0a0-0000-7000-8000-000000000003")
        .await
        .unwrap();
}

#[tokio::test]
async fn reject_team_grant_request_deletes_request_id() {
    let (_s, client) = serve(
        "DELETE",
        "teams/ops/requests/0192f0a0-0000-7000-8000-000000000003",
        json!("denied"),
    )
    .await;
    client
        .teams()
        .reject_team_grant_request("ops", "0192f0a0-0000-7000-8000-000000000003")
        .await
        .unwrap();
}

#[tokio::test]
async fn list_projects_and_caches_granting_team_decodes_grants() {
    let (_s, client) = serve(
        "GET",
        "teams/ops/grants",
        json!({
            "projects": [{"project": "web", "display_name": "Web", "users": true, "workers": true}],
            "caches": [{"cache": "main", "display_name": "Main"}]
        }),
    )
    .await;
    let grants = client
        .teams()
        .list_projects_and_caches_granting_team("ops")
        .await
        .unwrap();
    assert_eq!(grants.projects[0].project, "web");
    assert_eq!(grants.caches[0].cache, "main");
}

#[tokio::test]
async fn list_team_workers_decodes_entries() {
    let (_s, client) = serve("GET", "teams/ops/workers", json!([worker_entry()])).await;
    let workers = client.teams().list_team_workers("ops").await.unwrap();
    assert_eq!(workers[0].display_name, "builder");
}

#[tokio::test]
async fn register_team_worker_returns_team_and_token() {
    let (_s, client) = serve(
        "POST",
        "teams/ops/workers",
        json!({"team": "00000000-0000-0000-0000-000000000002", "token": "tok"}),
    )
    .await;
    let registered = client
        .teams()
        .register_team_worker(
            "ops",
            RegisterWorkerRequest {
                worker_id: "00000000-0000-0000-0000-000000000001".into(),
                display_name: "builder".into(),
                url: None,
                token: None,
                enable_network: None,
                enable_secrets: None,
                enable_exec: None,
                enable_eval: None,
                enable_build: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(registered.token.as_deref(), Some("tok"));
}

#[tokio::test]
async fn update_team_worker_patches_worker() {
    let (_s, client) = serve(
        "PATCH",
        "teams/ops/workers/00000000-0000-0000-0000-000000000001",
        json!("updated"),
    )
    .await;
    client
        .teams()
        .update_team_worker(
            "ops",
            "00000000-0000-0000-0000-000000000001",
            PatchWorkerRequest {
                active: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn delete_team_worker() {
    let (_s, client) = serve(
        "DELETE",
        "teams/ops/workers/00000000-0000-0000-0000-000000000001",
        json!("deleted"),
    )
    .await;
    client
        .teams()
        .delete_team_worker("ops", "00000000-0000-0000-0000-000000000001")
        .await
        .unwrap();
}

#[tokio::test]
async fn missing_capabilities_passes_projects_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/teams/ops/workers/demand"))
        .and(query_param("projects", "web,api"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "error": false,
            "message": {
                "builds": [{"architecture": "aarch64-linux", "required_features": ["kvm"], "build_count": 3}],
                "network": 1,
                "secrets": 0,
                "eval": 2
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let demand = client
        .teams()
        .get_team_workers_demand_missing_capabilities("ops", Some("web,api"))
        .await
        .unwrap();
    assert_eq!(demand.builds[0].build_count, 3);
    assert_eq!(demand.eval, 2);
}
