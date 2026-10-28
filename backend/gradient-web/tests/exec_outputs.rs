/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_entity::exec_job::ExecJobStatus;
use gradient_entity::ids::{DerivationId, EntryPointId, EvaluationId, ExecJobId};
use gradient_entity::{entry_point, exec_job, exec_job_output, project, project_access};
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{eval_at, public_project, task, user};
use gradient_fixtures::test_date;
use gradient_storage::NarStore;
use gradient_test_support::state::TestState;
use gradient_test_support::web::{make_test_server, signed_in_as};
use gradient_types::exec::ExecSpec;
use sea_orm::{MockDatabase, Value};
use serde_json::Value as Json;
use std::collections::BTreeMap;
use uuid::Uuid;

type Row = BTreeMap<&'static str, Value>;

fn job_id() -> ExecJobId {
    ExecJobId::new(Uuid::from_u128(0x7a))
}

fn evaluation_id() -> EvaluationId {
    EvaluationId::new(Uuid::from_u128(0xe1))
}

fn job() -> exec_job::Model {
    exec_job::Model {
        id: job_id(),
        evaluation: evaluation_id(),
        entry_point: EntryPointId::new(Uuid::from_u128(0xe9)),
        program_derivation: DerivationId::new(Uuid::from_u128(0xd1)),
        spec: serde_json::to_value(ExecSpec::default()).expect("spec json"),
        status: ExecJobStatus::Running,
        created_at: test_date(),
        ..Default::default()
    }
}

fn token_row(revoked: bool) -> Row {
    BTreeMap::from([
        ("attempt_number", Value::from(1)),
        ("created_at", Value::from(test_date())),
        ("expires_at", Value::from(test_date())),
        ("revoked", Value::from(revoked)),
    ])
}

fn with_job(db: MockDatabase, tokens: Vec<Row>, project: project::Model) -> MockDatabase {
    db.append_query_results([vec![job()]])
        .append_query_results([vec![entry_point::Model {
            eval: "apps.x86_64-linux.update".into(),
            ..Default::default()
        }]])
        .append_query_results([vec![eval_at(evaluation_id(), 0)]])
        .append_query_results([vec![task()]])
        .append_query_results([Vec::<gradient_entity::exec_job_attempt::Model>::new()])
        .append_query_results([Vec::<Row>::new()])
        .append_query_results([tokens])
        .append_query_results([Vec::<Row>::new()])
        .append_query_results([vec![task()]])
        .append_query_results([vec![project]])
}

fn output(path: &str, size: i64) -> exec_job_output::Model {
    exec_job_output::Model {
        exec_job: job_id(),
        path: path.into(),
        size,
        content_type: "text/plain".into(),
        created_at: test_date(),
        updated_at: test_date(),
    }
}

async fn adopt(store: &NarStore, contents: &str) {
    let staged = tempfile::NamedTempFile::new().expect("staged file");
    std::fs::write(staged.path(), contents).expect("write staged file");
    let (_, staged) = staged.keep().expect("keep staged file");
    store
        .adopt_exec_output(job_id().into_inner(), "log.txt", &staged)
        .await
        .expect("adopt output");
}

#[tokio::test]
async fn the_outputs_list_shows_live_sizes() {
    let db = with_job(mock_db(), vec![], public_project())
        .append_query_results([vec![output("a.txt", 3), output("b/c.bin", 4096)]]);

    let res = make_test_server(db.into_connection())
        .get(&format!("/api/v1/exec-jobs/{}/outputs", job_id()))
        .await;

    res.assert_status_ok();
    let body: Json = res.json();
    assert_eq!(body["message"][0]["path"], "a.txt");
    assert_eq!(body["message"][0]["size"], 3);
    assert_eq!(body["message"][1]["size"], 4096);
}

#[tokio::test]
async fn a_range_past_a_growing_output_answers_416_until_it_grows() {
    let base = tempfile::tempdir().expect("store dir");
    let store = NarStore::local(base.path()).expect("local store");
    adopt(&store, "abc").await;
    let db = with_job(mock_db(), vec![], public_project())
        .append_query_results([vec![output("log.txt", 3)]]);
    let db =
        with_job(db, vec![], public_project()).append_query_results([vec![output("log.txt", 6)]]);
    let server = TestState::new()
        .web_db(db.into_connection())
        .nar_storage(store.clone())
        .server();
    let url = format!("/api/v1/exec-jobs/{}/outputs/log.txt", job_id());

    let res = server.get(&url).add_header("range", "bytes=4-").await;
    assert_eq!(res.status_code(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(res.header("content-range"), "bytes */3");

    adopt(&store, "abcdef").await;
    let res = server.get(&url).add_header("range", "bytes=4-").await;
    assert_eq!(res.status_code(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(res.header("content-range"), "bytes 4-5/6");
    assert_eq!(res.text(), "ef");
}

#[tokio::test]
async fn an_outsider_cannot_read_a_private_jobs_outputs() {
    let (db, auth) = signed_in_as(user());
    let db = with_job(db, vec![], gradient_fixtures::rows::project())
        .append_query_results([Vec::<project_access::Model>::new()]);

    let res = make_test_server(db.into_connection())
        .get(&format!("/api/v1/exec-jobs/{}/outputs", job_id()))
        .add_header("authorization", auth)
        .await;

    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_job_page_lists_tokens_without_their_value() {
    let db = with_job(mock_db(), vec![token_row(true)], public_project())
        .append_query_results([vec![output("a.txt", 3), output("b.txt", 4)]]);

    let res = make_test_server(db.into_connection())
        .get(&format!("/api/v1/exec-jobs/{}", job_id()))
        .await;

    res.assert_status_ok();
    let body: Json = res.json();
    assert_eq!(body["message"]["tokens"][0]["revoked"], true);
    assert_eq!(body["message"]["outputs_size"], 7);
    assert!(!res.text().contains("GRJ_"));
}
