/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::task;
use gradient_sources::check_task_updates;
use gradient_test_support::state::test_state;
use sea_orm::{DatabaseBackend, MockDatabase};

/// `git://` URLs are using the pure-Rust pkt-line path. Connecting to an unbound loopback port is
/// then failing with connection refused without leaving loopback.
#[test]
fn check_task_updates_propagates_unreachable_remote_error() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
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
