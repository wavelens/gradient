/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::actions::send::matrix::{MatrixRoom, matrix_txn_id, post_matrix_message};
use crate::actions::send::slack::post_slack_message;
use crate::actions::summary::EventSummary;
use gradient_types::TaskActionId;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn http() -> reqwest::Client {
    gradient_util::http::build_client().expect("http client")
}

fn summary() -> EventSummary {
    EventSummary {
        event: "build.failed".into(),
        status: "failed".into(),
        project: Some("web".into()),
        task: Some("app".into()),
        ..Default::default()
    }
}

#[tokio::test]
async fn slack_transport_error_hides_the_webhook_url() {
    let webhook = "http://127.0.0.1:1/services/T0/B0/SECRET".parse().unwrap();
    let Err(err) = post_slack_message(&http(), webhook, &summary()).await else {
        panic!("nothing listens on port 1");
    };
    assert!(!format!("{err:#}").contains("SECRET"), "{err:#}");
}

#[tokio::test]
async fn matrix_puts_an_html_message_under_a_path_prefix() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path(
            "/matrix/_matrix/client/v3/rooms/!room:example.org/send/m.room.message/txn1",
        ))
        .and(header("authorization", "Bearer syt_secret"))
        .and(body_json(serde_json::json!({
            "msgtype": "m.text",
            "body": "web/app failed",
            "format": "org.matrix.custom.html",
            "formatted_body": "web/app failed",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"event_id":"$e"}"#))
        .expect(1)
        .mount(&server)
        .await;

    let room = MatrixRoom {
        homeserver: format!("{}/matrix/", server.uri()).parse().unwrap(),
        room_id: "!room:example.org",
        access_token: "syt_secret",
    };
    let ok = post_matrix_message(&http(), &room, "txn1", &summary())
        .await
        .unwrap();
    assert_eq!(ok.status_code, Some(200));
}

#[tokio::test]
async fn matrix_server_error_is_retried() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let room = MatrixRoom {
        homeserver: server.uri().parse().unwrap(),
        room_id: "!r:x",
        access_token: "t",
    };
    assert!(
        post_matrix_message(&http(), &room, "t", &summary())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn slack_rate_limit_is_retried_and_forbidden_is_recorded() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/limited"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/revoked"))
        .and(body_json(serde_json::json!({ "text": "web/app failed" })))
        .respond_with(ResponseTemplate::new(403).set_body_string("invalid_token"))
        .mount(&server)
        .await;

    let http = http();
    let limited = format!("{}/limited", server.uri()).parse().unwrap();
    assert!(
        post_slack_message(&http, limited, &summary())
            .await
            .is_err()
    );

    let revoked = format!("{}/revoked", server.uri()).parse().unwrap();
    let ok = post_slack_message(&http, revoked, &summary())
        .await
        .unwrap();
    assert_eq!(ok.status_code, Some(403));
    assert_eq!(ok.response_body.as_deref(), Some("invalid_token"));
}

#[test]
fn txn_id_is_stable_per_delivery() {
    let action = TaskActionId::now_v7();
    let a = matrix_txn_id(action, "build.failed", "2026-10-02T12:00:00Z");
    assert_eq!(
        a,
        matrix_txn_id(action, "build.failed", "2026-10-02T12:00:00Z")
    );
    assert_ne!(
        a,
        matrix_txn_id(action, "build.failed", "2026-10-02T12:00:01Z")
    );
}
