/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_entity::ids::*;
use gradient_entity::{
    EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH, RotatedHashMapping, UnresolvableProjectMapping,
};
use gradient_fixtures::ids::{project_id, task_id};
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{eval_at, public_project};
use gradient_test_support::state::TestState;
use sea_orm::{DatabaseConnection, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use uuid::Uuid;

fn build_job_id() -> BuildJobId {
    BuildJobId::new(Uuid::from_u128(0xb1))
}
fn derivation_id() -> DerivationId {
    DerivationId::new(Uuid::from_u128(0xd1))
}
fn shared_build_id() -> DerivationBuildId {
    DerivationBuildId::new(Uuid::from_u128(0xa1))
}
fn eval_id() -> EvaluationId {
    EvaluationId::new(Uuid::from_u128(0xe1))
}

fn build_rows() -> DatabaseConnection {
    mock_db()
        .append_query_results([vec![gradient_entity::build_job::Model {
            id: build_job_id(),
            evaluation: eval_id(),
            derivation: derivation_id(),
            derivation_build: shared_build_id(),
            ..Default::default()
        }]])
        .append_query_results([vec![gradient_entity::derivation_build::Model {
            id: shared_build_id(),
            derivation: derivation_id(),
            ..Default::default()
        }]])
        .append_query_results([vec![eval_at(eval_id(), 0)]])
        .append_query_results([vec![gradient_entity::task::Model {
            id: task_id(),
            project: project_id(),
            ..Default::default()
        }]])
        .append_query_results([vec![public_project()]])
        .append_query_results([vec![gradient_entity::derivation::Model {
            id: derivation_id(),
            hash: EXAMPLE_STORED_HASH.into(),
            name: "hello".into(),
            ..Default::default()
        }]])
        .append_query_results([vec![gradient_entity::derivation_output::Model {
            derivation: derivation_id(),
            name: "out".into(),
            hash: Some(EXAMPLE_STORED_HASH.into()),
            package: Some("hello".into()),
            ..Default::default()
        }]])
        .append_query_results([Vec::<gradient_entity::build_attempt::Model>::new()])
        .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
        .into_connection()
}

#[tokio::test]
async fn a_build_names_its_original_derivation_path() {
    let original = EXAMPLE_ORIGINAL_HASH;
    let db = build_rows();
    let server = TestState::new()
        .web_db(db)
        .store_path_mapping(Arc::new(RotatedHashMapping))
        .server();

    let response = server
        .get(&format!("/api/v1/builds/{}", build_job_id()))
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert_eq!(
        body["message"]["derivation_path"],
        format!("{original}-hello.drv")
    );
    assert_eq!(
        body["message"]["output"]["out"],
        format!("{original}-hello")
    );
}

#[tokio::test]
async fn a_build_of_a_project_that_does_not_resolve_is_unavailable() {
    let server = TestState::new()
        .web_db(build_rows())
        .store_path_mapping(Arc::new(UnresolvableProjectMapping))
        .server();

    let response = server
        .get(&format!("/api/v1/builds/{}", build_job_id()))
        .await;

    response.assert_status(StatusCode::SERVICE_UNAVAILABLE);
    assert!(!response.text().contains(EXAMPLE_STORED_HASH));
}
