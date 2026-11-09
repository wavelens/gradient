// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use connector::Client;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

#[tokio::test]
async fn basic_login_returns_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/basic/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok("abc")))
        .mount(&server)
        .await;

    let client = Client::builder().base_url(server.uri()).build().unwrap();
    let token = client
        .auth()
        .basic_login(connector::api::MakeLoginRequest {
            loginname: "user".into(),
            password: "pass".into(),
            remember_me: None,
        })
        .await
        .unwrap();
    assert_eq!(token, "abc");
}

#[tokio::test]
async fn logout_posts_with_the_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/logout"))
        .and(wiremock::matchers::header("authorization", "Bearer t"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok("Logged out")))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    client.auth().logout().await.unwrap();
}
