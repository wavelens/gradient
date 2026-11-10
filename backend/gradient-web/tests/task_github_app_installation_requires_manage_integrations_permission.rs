/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::role;
use gradient_fixtures::mock::{exec_ok, mock_db};
use gradient_fixtures::rows::{admin_role, project, project_member, task};
use gradient_test_support::state::TestState;
use gradient_test_support::web::{make_token, with_auth};
use gradient_types::SessionId;
use gradient_types::consts::BASE_ROLE_ADMIN_ID;
use gradient_types::permissions::{Permission, mask_from};
use serde_json::json;
use std::io::Write;
use std::sync::{Arc, Mutex};
use tracing_subscriber::fmt::MakeWriter;

#[derive(Clone, Default)]
struct LogLines(Arc<Mutex<Vec<u8>>>);

impl Write for LogLines {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("log buffer").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for LogLines {
    type Writer = LogLines;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn edit_only_role() -> role::Model {
    role::Model {
        permission: mask_from(&[Permission::ViewProject, Permission::EditTask]),
        ..admin_role()
    }
}

async fn patch_repository_to_github_as(role: role::Model) -> String {
    let session_id = SessionId::now_v7();
    let db = with_auth(mock_db(), session_id)
        .append_query_results([vec![project()]])
        .append_query_results([vec![task()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![role.clone()]])
        .append_query_results([vec![task()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![role]])
        .append_exec_results([exec_ok(), exec_ok()]);
    let server = TestState::new()
        .web_db(db.into_connection())
        .cli(|cli| {
            cli.github_app.id = std::num::NonZeroU64::new(1234);
            cli.github_app.private_key_file = Some("/dev/null".into());
            cli.github_app.webhook_secret_file = Some("/dev/null".into());
        })
        .server();

    let logs = LogLines::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    server
        .patch("/api/v1/tasks/test-project/test-task")
        .add_header(
            "authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .json(&json!({ "repository": "https://github.com/victim-org/app" }))
        .await
        .assert_status_ok();

    let lines = logs.0.lock().expect("log buffer").clone();
    String::from_utf8(lines).expect("utf-8 logs")
}

const GITHUB_APP_LOOKUP_FAILED: &str = "GitHub App's installation";

#[tokio::test]
async fn editing_the_repository_without_manage_integrations_never_looks_up_the_github_app() {
    let logs = patch_repository_to_github_as(edit_only_role()).await;
    assert!(!logs.contains(GITHUB_APP_LOOKUP_FAILED), "{logs}");
}

#[tokio::test]
async fn editing_the_repository_with_manage_integrations_looks_up_the_github_app() {
    let logs = patch_repository_to_github_as(admin_role()).await;
    assert!(logs.contains(GITHUB_APP_LOOKUP_FAILED), "{logs}");
}
