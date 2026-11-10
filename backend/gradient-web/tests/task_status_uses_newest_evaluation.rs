/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::projects::{
    ProjectEvaluationProgressCounts, get_running_and_waiting_evaluation_counts_by_project_id,
};
use gradient_entity::evaluation::{EvaluationKind, EvaluationStatus};
use gradient_entity::ids::EvaluationId;
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::rows;
use gradient_test_support::web::make_test_server;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, IntoActiveModel};
use serde_json::{Value, json};

#[tokio::test]
async fn task_endpoints_report_newest_evaluating_ssh_build_status_and_project_count_when_last_evaluation_column_points_to_older_failed_evaluation()
 {
    let db = TestDatabase::create().await;
    let conn = db.connection();
    rows::user()
        .into_active_model()
        .insert(&conn)
        .await
        .unwrap();
    let project = rows::public_project()
        .into_active_model()
        .insert(&conn)
        .await
        .unwrap();
    let task = rows::task()
        .into_active_model()
        .insert(&conn)
        .await
        .unwrap();
    let commit = rows::build_request_commit()
        .into_active_model()
        .insert(&conn)
        .await
        .unwrap();
    let failed = MEvaluation {
        commit: commit.id,
        status: EvaluationStatus::Failed,
        ..rows::eval_at(EvaluationId::now_v7(), 0)
    }
    .into_active_model()
    .insert(&conn)
    .await
    .unwrap();
    let mut pointed_at_failed: ATask = task.into();
    pointed_at_failed.last_evaluation = Set(Some(failed.id));
    pointed_at_failed.update(&conn).await.unwrap();
    let ssh_build = MEvaluation {
        commit: commit.id,
        status: EvaluationStatus::EvaluatingDerivation,
        kind: EvaluationKind::Ssh,
        ..rows::eval_at(EvaluationId::now_v7(), 60)
    }
    .into_active_model()
    .insert(&conn)
    .await
    .unwrap();

    let counts = get_running_and_waiting_evaluation_counts_by_project_id(&conn, &[project.id])
        .await
        .unwrap();
    assert_eq!(
        counts[&project.id],
        ProjectEvaluationProgressCounts {
            running: 1,
            waiting: 0
        }
    );

    let server = make_test_server(conn);
    let listed = server.get("/api/v1/tasks/test-project").await;
    listed.assert_status_ok();
    let shown = server.get("/api/v1/tasks/test-project/test-task").await;
    shown.assert_status_ok();

    let newest = json!({
        "last_evaluation": ssh_build.id,
        "last_evaluation_status": "EvaluatingDerivation",
    });
    for task in [
        listed.json::<Value>()["message"]["items"][0].clone(),
        shown.json::<Value>()["message"].clone(),
    ] {
        assert_eq!(
            json!({
                "last_evaluation": task["last_evaluation"],
                "last_evaluation_status": task["last_evaluation_status"],
            }),
            newest
        );
    }
    db.finish().await;
}
