/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use axum_test::TestServer;
use gradient_entity::ids::RoleId;
use gradient_entity::role;
use gradient_fixtures::ids::project_id;
use gradient_fixtures::rows::{project, project_member, user, view_role, write_role};
use gradient_test_support::state::TestState;
use gradient_test_support::web::{make_test_server, signed_in_as};
use gradient_types::permissions::{Permission, mask_from};
use sea_orm::MockDatabase;
use serde_json::json;
use uuid::Uuid;

const PROJECT: &str = "/api/v1/projects/test-project";
const CONNECTION: &str = "00000000-0000-0000-0000-000000000033";

fn member_with(role: &role::Model) -> (MockDatabase, String) {
    let (db, auth) = signed_in_as(user());
    let db = db
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(role.id)]])
        .append_query_results([vec![role.clone()]]);
    (db, auth)
}

async fn assert_forbidden(server: &TestServer, auth: String, method: &str, url: &str) {
    let request = match method {
        "PUT" => server.put(url).json(&json!({
            "name": "rogue",
            "git_host_type": "forgejo",
            "secret": "chosen-by-the-viewer",
        })),
        "PATCH" => server.patch(url).json(&json!({ "display_name": "rogue" })),
        "DELETE" => server.delete(url),
        _ => server.post(url),
    };
    let res = request.add_header("authorization", auth).await;

    assert_eq!(res.status_code(), StatusCode::FORBIDDEN, "{method} {url}");
}

#[tokio::test]
async fn a_view_member_cannot_change_integrations_ssh_key_or_subscriptions() {
    let connections = format!("{PROJECT}/git-host-connections");
    let connection = format!("{connections}/{CONNECTION}");
    let subscription = format!("{PROJECT}/subscribe/test-cache");
    for (method, url) in [
        ("PUT", connections.clone()),
        ("PATCH", connection.clone()),
        ("DELETE", connection.clone()),
        ("POST", format!("{PROJECT}/ssh")),
        ("POST", subscription.clone()),
        ("DELETE", subscription.clone()),
    ] {
        let (db, auth) = member_with(&view_role());
        let server = make_test_server(db.into_connection());

        assert_forbidden(&server, auth, method, &url).await;
    }
}

#[tokio::test]
async fn a_write_member_regenerates_the_ssh_key() {
    let (db, auth) = member_with(&write_role());
    let db = db.append_query_results([vec![project()]]);
    let crypt_file = std::env::temp_dir().join(format!("gradient-test-crypt-{}", Uuid::now_v7()));
    std::fs::write(&crypt_file, "this-is-a-32-byte-crypt-key!!!!").expect("write temp secret");
    let server = TestState::new()
        .web_db(db.into_connection())
        .cli(|cli| cli.secrets.crypt_file = crypt_file.to_string_lossy().into_owned())
        .server();

    let res = server
        .post(&format!("{PROJECT}/ssh"))
        .add_header("authorization", auth)
        .await;

    res.assert_status_ok();
}

#[tokio::test]
async fn project_settings_alone_do_not_regenerate_the_ssh_key() {
    let settings_only = role::Model {
        id: RoleId::new(Uuid::from_u128(0x5e)),
        name: "settings".into(),
        project: Some(project_id()),
        permission: mask_from(&[Permission::ViewProject, Permission::ManageProjectSettings]),
        managed: false,
    };
    let (db, auth) = member_with(&settings_only);
    let db = db
        .append_query_results([vec![project_member(settings_only.id)]])
        .append_query_results([vec![settings_only.clone()]]);
    let server = make_test_server(db.into_connection());

    assert_forbidden(&server, auth, "POST", &format!("{PROJECT}/ssh")).await;
}
