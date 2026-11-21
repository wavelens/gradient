/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_entity::ids::*;
use gradient_entity::project_access;
use gradient_fixtures::ids::{project_id, task_id};
use gradient_fixtures::rows::{admin_role, eval_at, project, superuser, task};
use gradient_test_support::web::{
    make_test_server, mock_db_and_auth_header_for_api_key_owned_by_user,
};
use gradient_types::permissions::{Permission, PermissionMask, mask_from};
use sea_orm::MockDatabase;
use serde_json::json;
use uuid::Uuid;

fn evaluation_id() -> EvaluationId {
    EvaluationId::new(Uuid::from_u128(0xe1))
}

fn build_job_id() -> BuildJobId {
    BuildJobId::new(Uuid::from_u128(0xb1))
}

fn with_evaluation(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![eval_at(evaluation_id(), 0)]])
        .append_query_results([vec![task()]])
        .append_query_results([vec![project()]])
}

fn with_build(db: MockDatabase) -> MockDatabase {
    let shared_build = DerivationBuildId::new(Uuid::from_u128(0xa1));
    db.append_query_results([vec![gradient_entity::build_job::Model {
        id: build_job_id(),
        evaluation: evaluation_id(),
        derivation_build: shared_build,
        ..Default::default()
    }]])
    .append_query_results([vec![gradient_entity::derivation_build::Model {
        id: shared_build,
        ..Default::default()
    }]])
    .append_query_results([vec![eval_at(evaluation_id(), 0)]])
    .append_query_results([vec![gradient_entity::task::Model {
        id: task_id(),
        project: project_id(),
        ..Default::default()
    }]])
    .append_query_results([vec![project()]])
}

fn with_admin_membership(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![project_access::Model {
        team: None,
        project: project_id(),
        user: superuser().id,
        role: admin_role().id,
    }]])
    .append_query_results([vec![admin_role()]])
}

fn without_membership(db: MockDatabase) -> MockDatabase {
    db.append_query_results([Vec::<project_access::Model>::new()])
}

async fn abort_status(
    build: bool,
    permission: PermissionMask,
    pin: Option<ProjectId>,
    membership: fn(MockDatabase) -> MockDatabase,
) -> StatusCode {
    let (db, auth) =
        mock_db_and_auth_header_for_api_key_owned_by_user(superuser(), permission, pin);
    let (db, url) = if build {
        (
            with_build(db),
            format!("/api/v1/builds/{}/abort", build_job_id()),
        )
    } else {
        (
            with_evaluation(db),
            format!("/api/v1/evals/{}", evaluation_id()),
        )
    };
    make_test_server(membership(db).into_connection())
        .post(&url)
        .add_header("authorization", auth)
        .json(&json!({ "method": "abort" }))
        .await
        .status_code()
}

fn narrowed() -> PermissionMask {
    mask_from(&[Permission::ViewProject, Permission::TriggerEvaluation])
}

#[tokio::test]
async fn a_narrowed_instance_admin_key_cannot_abort_work_of_a_foreign_private_project() {
    for build in [false, true] {
        assert_eq!(
            abort_status(build, narrowed(), None, without_membership).await,
            StatusCode::NOT_FOUND,
            "build={build}"
        );
    }
}

#[tokio::test]
async fn an_instance_admin_key_pinned_to_another_project_cannot_abort_work_of_this_project() {
    let other = ProjectId::new(Uuid::from_u128(0x0123));
    for build in [false, true] {
        assert_eq!(
            abort_status(build, mask_from(Permission::ALL), Some(other), |db| db).await,
            StatusCode::NOT_FOUND,
            "build={build}"
        );
    }
}

#[tokio::test]
async fn a_view_only_instance_admin_key_of_a_project_admin_cannot_abort() {
    let view_only = mask_from(&[Permission::ViewProject]);
    for build in [false, true] {
        assert_eq!(
            abort_status(build, view_only, None, with_admin_membership).await,
            StatusCode::FORBIDDEN,
            "build={build}"
        );
    }
}

#[tokio::test]
async fn an_instance_admin_key_narrowed_to_view_project_and_read_superuser_cannot_abort_work_of_a_foreign_private_project()
 {
    let read_superuser_only = mask_from(&[Permission::ViewProject, Permission::ReadSuperuser]);
    for build in [false, true] {
        assert_eq!(
            abort_status(build, read_superuser_only, None, without_membership).await,
            StatusCode::NOT_FOUND,
            "build={build}"
        );
    }
}

#[tokio::test]
async fn a_full_instance_admin_key_aborts_work_of_a_foreign_project() {
    for build in [false, true] {
        let status = abort_status(build, mask_from(Permission::ALL), None, |db| db).await;
        assert_ne!(status, StatusCode::NOT_FOUND, "build={build}");
        assert_ne!(status, StatusCode::FORBIDDEN, "build={build}");
    }
}
