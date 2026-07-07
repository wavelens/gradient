/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Tests for `check_project_updates` error propagation.
//!
//! Issue #280: manual `POST /evaluate` used to swallow git fetch failures
//! (DNS, connection refused, …) and bubble up a generic 500. The fix
//! propagates the `SourceError` to the caller so the web layer can return
//! a 4xx with a useful message.

use gradient_entity::project;
use gradient_sources::check_project_updates;
use gradient_test_support::state::test_state;
use sea_orm::{DatabaseBackend, MockDatabase};

/// `git://` URLs use the pure-Rust pkt-line path, so `TcpStream::connect`
/// to an unbound loopback port returns "connection refused" without
/// touching the network beyond loopback. Before the fix, this case was
/// hidden behind `Ok((false, vec![]))`.
#[test]
fn check_project_updates_propagates_unreachable_remote_error() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let state = test_state(db);
        let project = project::Model {
            repository: "git://127.0.0.1:1/nonexistent.git".into(),
            force_evaluation: true,
            ..Default::default()
        };

        let result = check_project_updates(&state.db(), &project, None).await;

        assert!(
            result.is_err(),
            "expected Err for unreachable git:// URL, got {:?}",
            result
        );
    });
}
