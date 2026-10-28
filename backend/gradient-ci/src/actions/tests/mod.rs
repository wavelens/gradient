/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod chat;
mod fixtures;
mod summary;

use super::matchers::{git_host_status_for_event, matches_event};
use super::matching_actions;
use super::payload::git_host_status_payload;
use super::report::build_ci_reports_from_payload;
use super::truncate;
use fixtures::{action_with, make_ctx, make_ctx_with, run};
use gradient_fixtures::mock::mock_db;
use gradient_git_host::reporter::CiStatus;
use gradient_types::ActionType;
use serde_json::json;

#[test]
fn git_host_status_mapping() {
    assert!(matches!(
        git_host_status_for_event("build.created"),
        Some(CiStatus::Pending)
    ));
    assert!(matches!(
        git_host_status_for_event("build.started"),
        Some(CiStatus::Running)
    ));
    assert!(matches!(
        git_host_status_for_event("build.completed"),
        Some(CiStatus::Success)
    ));
    assert!(matches!(
        git_host_status_for_event("build.failed"),
        Some(CiStatus::Failure)
    ));
    assert!(matches!(
        git_host_status_for_event("evaluation.queued"),
        Some(CiStatus::Pending)
    ));
    assert!(matches!(
        git_host_status_for_event("evaluation.building"),
        Some(CiStatus::Success)
    ));
    assert!(matches!(
        git_host_status_for_event("evaluation.completed"),
        Some(CiStatus::Success)
    ));
    assert!(matches!(
        git_host_status_for_event("evaluation.failed"),
        Some(CiStatus::Failure)
    ));
    assert!(matches!(
        git_host_status_for_event("evaluation.action_required"),
        Some(CiStatus::ActionRequired)
    ));
    assert!(git_host_status_for_event("evaluation.waiting").is_none());
}

#[test]
fn matches_event_send_mail_filters_by_stored_events() {
    let a = action_with(ActionType::SendMail, vec!["build.completed"]);
    assert!(matches_event(&a, "build.completed"));
    assert!(!matches_event(&a, "build.failed"));
}

fn open_pr(gate: gradient_types::VerifyGate) -> gradient_types::MTaskAction {
    use gradient_types::{ActionConfig, IntegrationId};

    let mut a = action_with(ActionType::OpenPr, vec![]);
    a.config = serde_json::to_value(ActionConfig::OpenPr {
        integration_id: IntegrationId::now_v7(),
        generator: Default::default(),
        granularity: Default::default(),
        verify_gate: gate,
        branch_pattern: "gradient/flake-lock-update".into(),
        title_template: None,
        body_template: None,
        update_existing: true,
    })
    .unwrap();

    a
}

#[test]
fn matches_event_open_pr_fires_only_on_gate_event() {
    use gradient_types::VerifyGate;

    // The gate is keying off the evaluation's own terminal transition, not a per-build event. A
    // candidate with an already-built closure is firing no `build.completed`. The evaluation is
    // still reaching `Building` or `Completed`.
    let build_gate = open_pr(VerifyGate::Build);
    assert!(matches_event(&build_gate, "evaluation.completed"));
    assert!(!matches_event(&build_gate, "evaluation.building"));
    assert!(!matches_event(&build_gate, "build.completed"));
    assert!(!matches_event(&build_gate, "evaluation.failed"));

    let eval_gate = open_pr(VerifyGate::Eval);
    assert!(matches_event(&eval_gate, "evaluation.building"));
    assert!(!matches_event(&eval_gate, "evaluation.completed"));
    assert!(!matches_event(&eval_gate, "build.completed"));
}

#[test]
fn matches_event_git_host_status_ignores_stored_events() {
    // The stored `events` list is irrelevant for `GitHostStatusReport` because
    // `GIT_HOST_STATUS_EVENTS` is driving matching. The row is seeded with an event outside that
    // set on purpose.
    let a = action_with(ActionType::GitHostStatusReport, vec!["evaluation.waiting"]);
    assert!(matches_event(&a, "build.created"));
    assert!(matches_event(&a, "build.queued"));
    assert!(matches_event(&a, "build.started"));
    assert!(matches_event(&a, "build.completed"));
    assert!(matches_event(&a, "build.failed"));
    assert!(matches_event(&a, "build.substituted"));
    assert!(matches_event(&a, "evaluation.queued"));
    assert!(matches_event(&a, "evaluation.building"));
    assert!(matches_event(&a, "evaluation.completed"));
    assert!(matches_event(&a, "evaluation.action_required"));
    assert!(matches_event(&a, "evaluation.approval_granted"));
    assert!(!matches_event(&a, "evaluation.waiting"));
}

#[test]
fn truncate_respects_max() {
    let s = "a".repeat(100);
    assert_eq!(truncate(s.clone(), 50).len(), 50);
    assert_eq!(truncate("short".into(), 50), "short");
}

#[test]
fn git_host_status_payload_includes_required_fields() {
    let p = git_host_status_payload("acme", "widgets", "deadbeef", "ctx", None, None, None);
    assert_eq!(p["owner"], "acme");
    assert_eq!(p["repo"], "widgets");
    assert_eq!(p["sha"], "deadbeef");
    assert_eq!(p["context"], "ctx");
    assert!(p.get("description").is_none());
}

#[test]
fn git_host_status_payload_includes_optional_fields() {
    let p = git_host_status_payload(
        "o",
        "r",
        "s",
        "c",
        Some("desc"),
        Some("https://x"),
        Some(42),
    );
    assert_eq!(p["description"], "desc");
    assert_eq!(p["details_url"], "https://x");
    assert_eq!(p["check_run_id"], 42);
}

#[test]
fn build_ci_report_fast_path_uses_payload_fields() {
    run(async {
        let ctx = make_ctx();
        let payload = json!({
            "owner": "acme",
            "repo": "widgets",
            "sha": "deadbeef",
            "context": "gradient/my-pkg",
            "description": "Building…",
            "details_url": "https://example.com/log/1",
            "check_run_id": 99,
        });
        let reports =
            build_ci_reports_from_payload(&ctx, "build.started", &payload, CiStatus::Running)
                .await
                .expect("fast path should succeed");
        let [report] = reports.as_slice() else {
            panic!("fast path emits exactly the payload's report: {reports:?}");
        };
        assert_eq!(report.owner, "acme");
        assert_eq!(report.repo, "widgets");
        assert_eq!(report.sha, "deadbeef");
        assert_eq!(report.context, "gradient/my-pkg");
        assert_eq!(report.description.as_deref(), Some("Building…"));
        assert_eq!(report.existing_check_id, Some(99));
    });
}

#[test]
fn build_ci_report_errors_when_payload_empty() {
    run(async {
        let ctx = make_ctx();
        let err =
            build_ci_reports_from_payload(&ctx, "build.started", &json!({}), CiStatus::Running)
                .await
                .unwrap_err();
        assert!(err.to_string().contains("build_id"), "error: {err}");
    });
}

#[test]
fn build_ci_report_errors_on_invalid_build_id() {
    run(async {
        let ctx = make_ctx();
        let payload = json!({ "build_id": "not-a-uuid" });
        let err = build_ci_reports_from_payload(&ctx, "build.started", &payload, CiStatus::Running)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("invalid build_id"), "error: {err}");
    });
}

#[test]
fn a_failed_attribute_reddens_the_evaluation_check_and_gets_its_own_build_check() {
    use gradient_entity::evaluation_message::MessageLevel;
    use gradient_types::{EvaluationId, MCommit, MEvaluation, MEvaluationMessage, MTask, TaskId};

    run(async {
        let evaluation_id = EvaluationId::now_v7();
        let task = TaskId::now_v7();
        let db = mock_db()
            .append_query_results([vec![MEvaluation {
                id: evaluation_id,
                task: Some(task),
                building_started_at: Some(gradient_types::now()),
                ..Default::default()
            }]])
            .append_query_results([vec![MTask {
                id: task,
                name: "hosts".into(),
                repository: "https://git.example.com/acme/infra".into(),
                ..Default::default()
            }]])
            .append_query_results([vec![MCommit {
                hash: vec![0xab, 0xcd],
                ..Default::default()
            }]])
            .append_query_results([Vec::<gradient_types::MProject>::new()])
            .append_query_results([vec![MEvaluationMessage {
                evaluation: evaluation_id,
                level: MessageLevel::Error,
                source: Some("nix-eval:nixosConfigurations.c".into()),
                message: "failed to evaluate 'nixosConfigurations.c': boom".into(),
                ..Default::default()
            }]]);
        let ctx = make_ctx_with(db);
        let payload = json!({ "evaluation_id": evaluation_id.to_string() });

        let reports =
            build_ci_reports_from_payload(&ctx, "evaluation.building", &payload, CiStatus::Success)
                .await
                .unwrap();

        let checks: Vec<(&str, &CiStatus)> = reports
            .iter()
            .map(|r| (r.context.as_str(), &r.status))
            .collect();
        assert_eq!(
            checks,
            vec![
                ("gradient/hosts: Evaluation", &CiStatus::Failure),
                (
                    "gradient/hosts: Build nixosConfigurations.c",
                    &CiStatus::Failure
                ),
            ]
        );
    });
}

#[test]
fn an_input_update_reaches_open_pr_and_never_the_git_host_report() {
    let actions = vec![
        open_pr(gradient_types::VerifyGate::Build),
        action_with(ActionType::GitHostStatusReport, vec![]),
        action_with(ActionType::SendMail, vec!["evaluation.completed"]),
    ];
    let payload = json!({"evaluation_kind": "input_update"});

    let matched = matching_actions(actions, "evaluation.completed", &payload);

    assert_eq!(
        matched.iter().map(|a| a.action_type).collect::<Vec<_>>(),
        vec![ActionType::OpenPr, ActionType::SendMail],
    );
}

#[test]
fn a_normal_run_reaches_the_git_host_report_and_never_open_pr() {
    let actions = vec![
        open_pr(gradient_types::VerifyGate::Build),
        action_with(ActionType::GitHostStatusReport, vec![]),
    ];
    let payload = json!({"evaluation_kind": "normal"});

    let matched = matching_actions(actions, "evaluation.completed", &payload);

    assert_eq!(
        matched.iter().map(|a| a.action_type).collect::<Vec<_>>(),
        vec![ActionType::GitHostStatusReport],
    );
}

#[test]
fn an_action_that_does_not_subscribe_is_not_matched() {
    let actions = vec![action_with(ActionType::SendMail, vec!["build.failed"])];

    assert!(matching_actions(actions, "build.completed", &json!({})).is_empty());
}

fn exec_tree() -> gradient_types::MTaskAction {
    use gradient_types::{ActionConfig, IntegrationId, PatchGeneratorKind};

    let mut a = action_with(ActionType::OpenPr, vec![]);
    a.config = serde_json::to_value(ActionConfig::OpenPr {
        integration_id: IntegrationId::now_v7(),
        generator: PatchGeneratorKind::ExecTree,
        granularity: Default::default(),
        verify_gate: Default::default(),
        branch_pattern: "gradient/{entry_point}".into(),
        title_template: None,
        body_template: None,
        update_existing: true,
    })
    .unwrap();
    a
}

#[test]
fn exec_tree_fires_on_exec_job_completed_only() {
    let kinds = |event: &str, payload: serde_json::Value| {
        matching_actions(
            vec![exec_tree(), open_pr(gradient_types::VerifyGate::Build)],
            event,
            &payload,
        )
        .iter()
        .map(|a| super::matchers::open_pr_gate_events(a).unwrap()[0])
        .collect::<Vec<_>>()
    };

    assert_eq!(
        kinds("exec_job.completed", json!({})),
        vec!["exec_job.completed"]
    );
    assert!(kinds("exec_job.failed", json!({})).is_empty());
    assert!(kinds("evaluation.completed", json!({})).is_empty());
    assert_eq!(
        kinds(
            "evaluation.completed",
            json!({"evaluation_kind": "input_update"})
        ),
        vec!["evaluation.completed"]
    );
}

#[test]
fn an_exec_tree_action_without_a_tree_patch_opens_nothing() {
    use gradient_entity::exec_job_output;
    use gradient_types::{ExecJobId, IntegrationId, TaskActionId, TaskId};

    for outputs in [
        vec![],
        vec![exec_job_output::Model {
            path: "gradient/tree.patch".into(),
            size: 0,
            ..Default::default()
        }],
    ] {
        run(async {
            let ctx = make_ctx_with(mock_db().append_query_results([outputs]));
            let payload = json!({ "job": ExecJobId::now_v7().to_string() });

            let ok = super::send::execute_exec_tree_pr(
                &ctx,
                &payload,
                TaskActionId::now_v7(),
                TaskId::now_v7(),
                IntegrationId::now_v7(),
                "gradient/{entry_point}",
                None,
                None,
                true,
            )
            .await
            .unwrap();

            assert_eq!(ok.status_code, Some(204));
        });
    }
}

#[test]
fn start_evaluation_queues_with_the_parent_job() {
    use gradient_entity::commit;
    use gradient_fixtures::mock::{exec_ok, raw_statements};
    use gradient_sources::HeadCommit;
    use gradient_types::{EvaluationId, ExecJobId, MEvaluation, MTask, TaskId};

    run(async {
        let task = MTask {
            id: TaskId::now_v7(),
            ..Default::default()
        };
        let job = ExecJobId::now_v7();
        let db = mock_db()
            .append_query_results([vec![commit::Model::default()]])
            .append_query_results([vec![MEvaluation {
                id: EvaluationId::now_v7(),
                concurrent: true,
                ..Default::default()
            }]])
            .append_query_results([Vec::<gradient_entity::task_flake_input_override::Model>::new()])
            .append_query_results([vec![task.clone()]])
            .append_exec_results([exec_ok()])
            .into_connection();
        let head = HeadCommit {
            hash: vec![0xab; 20],
            message: "m".into(),
            author_email: None,
            author_name: "a".into(),
        };

        super::send::queue_evaluation(&db, &task, head, Some(job))
            .await
            .unwrap();

        let log = raw_statements(db.into_transaction_log());
        let insert = log
            .iter()
            .find(|s| s.sql.starts_with(r#"INSERT INTO "evaluation""#))
            .expect("the evaluation is inserted");
        let columns: Vec<&str> = insert.sql
            [insert.sql.find('(').unwrap() + 1..insert.sql.find(')').unwrap()]
            .split(", ")
            .collect();
        let concurrent = columns
            .iter()
            .position(|c| *c == r#""concurrent""#)
            .unwrap();
        assert_eq!(
            insert.values.as_ref().unwrap().0[concurrent],
            sea_orm::Value::Bool(Some(true))
        );
        let parent = log
            .iter()
            .find(|s| {
                s.sql.starts_with(r#"UPDATE "evaluation""#) && s.sql.contains("parent_exec_job")
            })
            .expect("the parent job is recorded");
        assert!(
            parent
                .values
                .as_ref()
                .unwrap()
                .0
                .contains(&sea_orm::Value::Uuid(Some(job.into_inner()))),
            "{parent:?}"
        );
    });
}

#[test]
fn start_evaluation_refuses_a_task_of_another_project() {
    use gradient_types::{MTask, ProjectId, TaskId};

    run(async {
        let owner = MTask {
            id: TaskId::now_v7(),
            project: ProjectId::now_v7(),
            ..Default::default()
        };
        let other = MTask {
            id: TaskId::now_v7(),
            project: ProjectId::now_v7(),
            ..Default::default()
        };
        let ctx = make_ctx_with(
            mock_db()
                .append_query_results([vec![owner.clone()]])
                .append_query_results([vec![other.clone()]]),
        );

        let refused = super::send::execute_start_evaluation(&ctx, &json!({}), owner.id, other.id)
            .await
            .err()
            .expect("a task of another project is refused");

        assert_eq!(
            refused.to_string(),
            "the task to start belongs to another project"
        );
    });
}

#[test]
fn exec_events_report_one_status_per_entry_point() {
    use gradient_types::{EvaluationId, ExecJobId, MCommit, MEvaluation, MTask, TaskId};

    for (event, status) in [
        ("exec_job.started", CiStatus::Running),
        ("exec_job.completed", CiStatus::Success),
        ("exec_job.failed", CiStatus::Failure),
        ("exec_job.aborted", CiStatus::Error),
    ] {
        assert_eq!(git_host_status_for_event(event), Some(status), "{event}");
    }

    run(async {
        let evaluation_id = EvaluationId::now_v7();
        let task = TaskId::now_v7();
        let db = mock_db()
            .append_query_results([vec![MEvaluation {
                id: evaluation_id,
                task: Some(task),
                ..Default::default()
            }]])
            .append_query_results([vec![MTask {
                id: task,
                name: "master".into(),
                repository: "https://git.example.com/acme/infra".into(),
                ..Default::default()
            }]])
            .append_query_results([vec![MCommit {
                hash: vec![0xab, 0xcd],
                ..Default::default()
            }]])
            .append_query_results([Vec::<gradient_types::MProject>::new()]);
        let ctx = make_ctx_with(db);
        let payload = json!({
            "job": ExecJobId::now_v7().to_string(),
            "evaluation": evaluation_id.to_string(),
            "entry_point_name": "apps.x86_64-linux.update",
        });

        let reports =
            build_ci_reports_from_payload(&ctx, "exec_job.completed", &payload, CiStatus::Success)
                .await
                .unwrap();

        let [report] = reports.as_slice() else {
            panic!("one report per exec job: {reports:?}");
        };
        assert_eq!(
            report.context,
            crate::build_check_context("master", "apps.x86_64-linux.update")
        );
        assert_eq!(report.sha, "abcd");
        assert_eq!(report.status, CiStatus::Success);
    });
}
