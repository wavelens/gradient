/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::Duration;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::{EvaluationId, TaskId};
use gradient_fixtures::shape::shape;
use gradient_fixtures::test_date;
use gradient_test_support::state::TestState;
use sea_orm::{ActiveModelTrait, IntoActiveModel};
use serde_json::{Value, json};

#[tokio::test]
async fn the_task_list_shows_the_last_five_runs_oldest_first_and_none_for_an_idle_task() {
    let runs: Vec<EvaluationId> = (0..6).map(|_| EvaluationId::now_v7()).collect();
    let mut declared = shape();
    for (hour, run) in (0_i64..).zip(&runs) {
        let created_at = test_date() + Duration::hours(hour);
        declared = declared.evaluation(*run);
        if hour > 0 {
            declared = declared.same_task_as(runs[0]);
        }
        declared = declared
            .evaluation_status(EvaluationStatus::Completed)
            .evaluation_option(|e| {
                e.created_at = created_at;
                e.fetch_started_at = Some(created_at + Duration::minutes(1));
                e.finished_at = Some(created_at + Duration::minutes(3));
            });
    }
    let db = declared.last_evaluation(runs[5]).into_database().await;
    db.execute("UPDATE project SET public = true").await;
    let project = db.rows::<gradient_entity::project::Entity>().await[0].clone();
    gradient_entity::task::Model {
        id: TaskId::now_v7(),
        project: project.id,
        name: "idle".into(),
        ..gradient_fixtures::rows::task()
    }
    .into_active_model()
    .insert(&db.connection())
    .await
    .expect("the idle task is written");
    let server = TestState::new().web_db(db.connection()).server();

    let response = server.get(&format!("/api/v1/tasks/{}", project.name)).await;

    response.assert_status_ok();
    let tasks = response.json::<Value>()["message"]["items"].clone();
    let recent = |name: &str| {
        tasks
            .as_array()
            .expect("the tasks are a list")
            .iter()
            .find(|task| task["name"] == json!(name))
            .map(|task| task["recent_evaluations"].clone())
            .expect("the task is listed")
    };
    let expected: Vec<Value> = (1..6)
        .zip(&runs[1..])
        .map(|(hour, run)| {
            let created_at = test_date() + Duration::hours(hour);
            json!({
                "id": run,
                "status": EvaluationStatus::Completed,
                "duration_ms": 120_000,
                "created_at": created_at,
                "started_at": created_at + Duration::minutes(1),
                "finished_at": created_at + Duration::minutes(3),
            })
        })
        .collect();
    let busy = db.rows::<gradient_entity::task::Entity>().await;
    let busy = busy
        .iter()
        .find(|task| task.name != "idle")
        .expect("the task with runs is written");
    assert_eq!(recent(&busy.name), json!(expected));
    assert_eq!(recent("idle"), json!([]));
    db.finish().await;
}
