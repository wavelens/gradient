/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::Utc;
use gradient_db::tasks::record_fetch;
use gradient_entity::task;
use gradient_fixtures::mock::{empty_db, exec_ok, mock_db, statements};
use gradient_sources::{SourceError, check_task_updates};
use gradient_test_support::state::{TestState, test_state};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// The git:// URL can go through the git subtransport in libgit2. That subtransport can connect
/// only to addresses in the admin allowlist. A loopback URL on port 1 can fail in one of two ways.
/// The allowlist can reject it, or the connection attempt can fail.
#[test]
fn check_task_updates_propagates_unreachable_remote_error() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let db = empty_db();
        let state = test_state(db);
        let task = task::Model {
            repository: "git://127.0.0.1:1/nonexistent.git".into(),
            force_evaluation: true,
            ..Default::default()
        };

        let result = check_task_updates(&state.db(), &task, None).await;

        assert!(
            result.is_err(),
            "expected Err for unreachable git:// URL, got {:?}",
            result
        );
    });
}

fn spawn_loopback_tcp_listener_counting_accepted_connections() -> (u16, Arc<AtomicUsize>) {
    let listener =
        TcpListener::bind("127.0.0.1:0").expect("Bind a loopback TCP listener for tests.");
    listener
        .set_nonblocking(true)
        .expect("Switch the test listener to non-blocking mode.");
    let port = listener
        .local_addr()
        .expect("Read the port of the test listener.")
        .port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&accepted);
    std::thread::spawn(move || {
        loop {
            match listener.accept() {
                Ok((connection, _)) => {
                    counter.fetch_add(1, Ordering::SeqCst);
                    drop(connection);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => return,
            }
        }
    });
    (port, accepted)
}

#[test]
fn test_git_task_update_for_127_0_0_1_and_localhost_with_empty_admin_allowlist_refuses_with_guarded_http_client_text_records_it_and_connects_to_no_listener()
 {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let (port, accepted) = spawn_loopback_tcp_listener_counting_accepted_connections();
        let state = TestState::new()
            .cli(|cli| cli.outbound.allowed_non_public_network_targets.clear())
            .build();

        for host in ["127.0.0.1", "localhost"] {
            let http_reason = state
                .user_supplied_url_http_client
                .request_builder_for_checked_url(
                    reqwest::Method::GET,
                    &format!("http://{host}:{port}/repo"),
                )
                .unwrap_err()
                .to_string();
            let task = task::Model {
                repository: format!("git://{host}:{port}/repo"),
                force_evaluation: true,
                ..Default::default()
            };

            let result = check_task_updates(&state.db(), &task, None).await;

            let Err(error @ SourceError::GitServerAddressRefusedByAdminAllowlist { .. }) = result
            else {
                panic!("Expected rejection from the admin allowlist, but got {result:?}.");
            };
            let reason = error.short_reason_for_user();
            assert_eq!(reason, http_reason, "{host}");
            if host == "localhost" {
                assert!(!reason.contains("127.0.0.1"), "{reason}");
            }
            let db = mock_db().append_exec_results([exec_ok()]).into_connection();
            record_fetch(&db, task.id, Utc::now().naive_utc(), Some(&reason))
                .await
                .unwrap();
            let statements = statements(db.into_transaction_log());
            assert!(
                statements
                    .iter()
                    .any(|statement| statement.contains(&reason)),
                "The fetch record for the task must keep the rejection text: {statements:?}"
            );
        }
        assert_eq!(
            accepted.load(Ordering::SeqCst),
            0,
            "The listener accepted the connection, but this address is not on the admin allowlist."
        );
    });
}

#[test]
fn check_task_updates_refuses_stored_repository_url_with_disallowed_scheme() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = test_state(empty_db());
        let task = task::Model {
            repository: "path:/x".into(),
            force_evaluation: true,
            ..Default::default()
        };

        let result = check_task_updates(&state.db(), &task, None).await;

        assert!(matches!(result, Err(SourceError::InvalidUrl)), "{result:?}");
    });
}
