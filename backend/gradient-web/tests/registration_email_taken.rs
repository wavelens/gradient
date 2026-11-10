/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::user;
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::user as existing_user;
use gradient_test_support::fakes::email::{InMemoryEmailSender, SentEmail};
use gradient_test_support::state::TestState;
use gradient_types::EmailConfig;
use serde_json::{Value, json};
use std::sync::Arc;

fn email_config(require_verification: bool) -> EmailConfig {
    EmailConfig {
        smtp_host: "localhost".into(),
        smtp_port: 25,
        smtp_username: String::new(),
        smtp_password_file: String::new(),
        from_address: "gradient@example.com".into(),
        from_name: "Gradient".into(),
        enable_tls: false,
        require_verification,
    }
}

fn registration_with_taken_email(
    require_verification: bool,
) -> (axum_test::TestServer, Arc<InMemoryEmailSender>) {
    let db = mock_db()
        .append_query_results([Vec::<user::Model>::new()])
        .append_query_results([vec![existing_user()]]);
    let email = Arc::new(InMemoryEmailSender::new());
    let server = TestState::new()
        .web_db(db.into_connection())
        .email(email.clone())
        .config(move |config| {
            config.registration.enable = true;
            config.email = Some(email_config(require_verification));
        })
        .server();
    (server, email)
}

async fn register(server: &axum_test::TestServer, email: &str) -> axum_test::TestResponse {
    server
        .post("/api/v1/auth/basic/register")
        .json(&json!({
            "username": "newcomer",
            "name": "Newcomer",
            "email": email,
            "password": "A-long-enough-passw0rd!",
        }))
        .await
}

#[tokio::test]
async fn a_taken_email_answers_like_a_new_account_and_mails_its_owner() {
    let (server, email) = registration_with_taken_email(true);
    let owner = existing_user();

    let res = register(&server, &owner.email).await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    let sent = email.sent.lock().unwrap();
    assert!(
        matches!(sent.as_slice(), [SentEmail::ActionMail { to, .. }] if to.as_slice() == std::slice::from_ref(&owner.email)),
        "{sent:?}"
    );
}

#[tokio::test]
async fn without_email_verification_a_taken_email_is_named() {
    let (server, _) = registration_with_taken_email(false);

    let res = register(&server, &existing_user().email).await;

    res.assert_status(axum::http::StatusCode::CONFLICT);
    let body: Value = res.json();
    assert_eq!(body["code"], "email_address_already_in_use", "{body}");
}
