/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::io::Write as _;
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::time::Duration;

use gradient_fixtures::mock::{exec_ok, mock_db, raw_statements};
use gradient_types::actions::ActionConfig;
use gradient_types::ids::PendingDeliveryId;
use gradient_types::{ActionType, MTaskAction, MTaskActionDelivery, MWebhook, MWebhookDelivery};
use gradient_util::http::HttpClientForUserSuppliedUrls;
use gradient_util::http_validation::UserUrlDestinationAllowlist;
use serde_json::json;

use super::fixtures::{action_with, make_ctx_with};
use crate::CiContext;

const REFUSAL_MESSAGE_SEEN_BY_TEAM_MEMBERS_FOR_INTERNAL_EXAMPLE_HOST: &str = "The server can't connect to internal.example. The name doesn't resolve, or the address is private and not allowed. An admin can allow the host in setting `outbound.allowedNonPublicNetworkTargets` (`GRADIENT_OUTBOUND_ALLOWED_NON_PUBLIC_NETWORK_TARGETS`).";

fn ci_context_with_mock_database_http_client_resolving_every_host_name_to_loopback(
    db: sea_orm::MockDatabase,
) -> CiContext {
    let mut ctx = make_ctx_with(db);
    ctx.db.user_supplied_url_http_client = HttpClientForUserSuppliedUrls::new(
        UserUrlDestinationAllowlist::default().with_dns_lookup_function(|_, port| {
            Box::pin(async move { Ok(vec![SocketAddr::from(([127, 0, 0, 1], port))]) })
        }),
    )
    .expect("http client");
    ctx
}

fn bind_nonblocking_loopback_listener_and_internal_example_hook_url() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    listener
        .set_nonblocking(true)
        .expect("non-blocking listener");
    let port = listener.local_addr().expect("local addr").port();
    (listener, format!("http://internal.example:{port}/hook"))
}

fn assert_no_client_connected_to_loopback_listener(listener: &TcpListener) {
    match listener.accept() {
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
        other => panic!("Delivery connected to loopback address: {other:?}"),
    }
}

fn assert_delivery_insert_records_generic_refusal(ctx: CiContext, delivery_table: &str) {
    let log = raw_statements(ctx.db.worker_db.into_transaction_log());
    let insert = log
        .iter()
        .find(|s| {
            s.sql
                .starts_with(&format!(r#"INSERT INTO "{delivery_table}""#))
        })
        .unwrap_or_else(|| panic!("no delivery recorded: {log:?}"));
    let texts: Vec<String> = insert
        .values
        .as_ref()
        .expect("insert values")
        .0
        .iter()
        .filter_map(|value| match value {
            sea_orm::Value::String(Some(text)) => Some(text.to_string()),
            _ => None,
        })
        .collect();
    assert!(
        texts
            .iter()
            .any(|t| t.contains(REFUSAL_MESSAGE_SEEN_BY_TEAM_MEMBERS_FOR_INTERNAL_EXAMPLE_HOST)),
        "{texts:?}"
    );
    assert!(
        !texts
            .iter()
            .any(|t| t.contains("127.0.0.1") || t.contains("not a public address")),
        "{texts:?}"
    );
    assert!(
        insert
            .values
            .as_ref()
            .expect("insert values")
            .0
            .contains(&sea_orm::Value::Bool(Some(false)))
    );
}

#[tokio::test]
async fn webhook_delivery_to_loopback_host_name_records_refusal_without_opening_connection() {
    let mut crypt_file = tempfile::NamedTempFile::new().expect("crypt file");
    crypt_file
        .write_all(b"test-secret-key-32-bytes-padding!")
        .expect("write crypt file");
    let mut ctx = ci_context_with_mock_database_http_client_resolving_every_host_name_to_loopback(
        mock_db().append_query_results([vec![MWebhookDelivery::default()]]),
    );
    let mut config = (*ctx.db.config).clone();
    config.secrets.crypt_file = crypt_file.path().to_string_lossy().into_owned();
    ctx.db.config = Arc::new(config);
    let (listener, url) = bind_nonblocking_loopback_listener_and_internal_example_hook_url();
    let hook = MWebhook {
        url,
        secret: crate::actions::encrypt_secret_with_file(
            &ctx.db.config.secrets.crypt_file,
            "webhook-signing-secret",
        )
        .expect("encrypt webhook secret"),
        ..Default::default()
    };

    let delivered = tokio::time::timeout(
        Duration::from_secs(5),
        crate::webhooks::deliver(
            &ctx,
            &hook,
            &json!({"event": "evaluation.failed"}),
            PendingDeliveryId::now_v7(),
        ),
    )
    .await;

    assert_no_client_connected_to_loopback_listener(&listener);
    assert!(matches!(delivered, Ok(Ok(_))), "{delivered:?}");
    assert_delivery_insert_records_generic_refusal(ctx, "webhook_delivery");
}

#[tokio::test]
async fn test_send_web_request_to_loopback_host_opens_no_connection_and_records_refusal_in_task_action_delivery()
 {
    let ctx = ci_context_with_mock_database_http_client_resolving_every_host_name_to_loopback(
        mock_db()
            .append_query_results([vec![MTaskActionDelivery::default()]])
            .append_exec_results([exec_ok()]),
    );
    let (listener, url) = bind_nonblocking_loopback_listener_and_internal_example_hook_url();
    let action = MTaskAction {
        config: serde_json::to_value(ActionConfig::SendWebRequest { url, token: None })
            .expect("action config"),
        ..action_with(ActionType::SendWebRequest, vec![])
    };

    let executed = tokio::time::timeout(
        Duration::from_secs(5),
        crate::actions::execute_action(&ctx, action, "evaluation.failed", json!({})),
    )
    .await;

    assert_no_client_connected_to_loopback_listener(&listener);
    assert!(matches!(executed, Ok(Err(_))), "{executed:?}");
    assert_delivery_insert_records_generic_refusal(ctx, "task_action_delivery");
}
