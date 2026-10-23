// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use connector::Client;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn config_returns_struct() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/config"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "error": false,
            "message": {
                "create_cache": "everyone",
                "create_project": "everyone",
                "create_team": "superusers",
                "email_verification_enabled": false,
                "github_app_enabled": false,
                "gradient_ci_enabled": false,
                "gradient_ci_url": "",
                "logo_url": null,
                "oidc_enabled": false,
                "oidc_required": false,
                "quic": false,
                "registration_enabled": true,
                "smtp_enabled": false,
                "ssh_enabled": true,
                "ssh_port": 22,
                "version": "2.0.0"
            }
        })))
        .mount(&server)
        .await;

    let client = Client::builder().base_url(server.uri()).build().unwrap();
    let cfg = client.server().get_config().await.unwrap();
    assert!(!cfg.oidc_enabled);
    assert!(cfg.registration_enabled);
}
