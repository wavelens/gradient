/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod chat;
mod fixtures;
mod summary;

use super::matchers::matches_event;
use super::matching_actions;
use super::report::build_ci_reports_from_commit_status_check_rows;
use super::truncate;
use crate::reporting::CommitStatusCheckWithIds;
use fixtures::{action_with, make_ctx_with, run};
use gradient_fixtures::mock::mock_db;
use gradient_git_host::reporter::CiStatus;
use gradient_types::ActionType;
use serde_json::json;

#[test]
fn matches_event_send_mail_filters_by_stored_events() {
    let a = action_with(ActionType::SendMail, vec!["build.completed"]);
    assert!(matches_event(&a, "build.completed"));
    assert!(!matches_event(&a, "build.failed"));
}

fn open_pr(gate: gradient_types::VerifyGate) -> gradient_types::MTaskAction {
    use gradient_types::{ActionConfig, GitHostConnectionId};

    let mut a = action_with(ActionType::OpenPr, vec![]);
    a.config = serde_json::to_value(ActionConfig::OpenPr {
        git_host_connection_id: GitHostConnectionId::now_v7(),
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
fn truncate_respects_max() {
    let s = "a".repeat(100);
    assert_eq!(truncate(s.clone(), 50).len(), 50);
    assert_eq!(truncate("short".into(), 50), "short");
}

fn append_task_commit_and_empty_project_result_to_mock_database(
    db: sea_orm::MockDatabase,
    task: gradient_types::TaskId,
) -> sea_orm::MockDatabase {
    use gradient_types::{MCommit, MTask};

    db.append_query_results([vec![MTask {
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
}

fn build_event_test_case_with_build_status(
    status: gradient_entity::build::BuildStatus,
    task: gradient_types::TaskId,
) -> (gradient_types::Event, sea_orm::MockDatabase, String) {
    use gradient_entity::build::BuildStatus;
    use gradient_types::events::build;
    use gradient_types::{
        BuildJobId, DerivationBuildId, DerivationId, EvaluationId, Event, MBuildJob,
        MDerivationBuild, MEntryPoint, MEvaluation,
    };

    let evaluation_id = EvaluationId::now_v7();
    let build_job = MBuildJob {
        id: BuildJobId::now_v7(),
        evaluation: evaluation_id,
        derivation: DerivationId::now_v7(),
        derivation_build: DerivationBuildId::now_v7(),
        ..Default::default()
    };
    let event = Event::from(build::Reported {
        build_id: build_job.id,
        evaluation_id,
        status: i16::from(status),
        ..Default::default()
    });
    let db = mock_db()
        .append_query_results([vec![MEvaluation {
            id: evaluation_id,
            task: Some(task),
            ..Default::default()
        }]])
        .append_query_results([vec![build_job.clone()]])
        .append_query_results([vec![MDerivationBuild {
            id: build_job.derivation_build,
            status: BuildStatus::Completed,
            ..Default::default()
        }]])
        .append_query_results([vec![MEntryPoint {
            evaluation: evaluation_id,
            derivation: build_job.derivation,
            eval: "packages.x86_64-linux.hello".into(),
            ..Default::default()
        }]]);
    (
        event,
        db,
        crate::build_check_context("hosts", "packages.x86_64-linux.hello"),
    )
}

fn build_exec_job_event_test_case(
    status: gradient_entity::exec_job::ExecJobStatus,
    task: gradient_types::TaskId,
) -> (gradient_types::Event, sea_orm::MockDatabase, String) {
    use gradient_entity::exec_job::ExecJobStatus;
    use gradient_types::events::exec_job;
    use gradient_types::{EvaluationId, Event, ExecJobId, MEvaluation, MExecJob};

    let evaluation_id = EvaluationId::now_v7();
    let job = ExecJobId::now_v7();
    let event = Event::from(exec_job::Reported {
        job,
        evaluation: evaluation_id,
        status: i16::from(status),
        entry_point_name: Some("apps.x86_64-linux.update".into()),
        ..Default::default()
    });
    let db = mock_db()
        .append_query_results([vec![MEvaluation {
            id: evaluation_id,
            task: Some(task),
            ..Default::default()
        }]])
        .append_query_results([vec![MExecJob {
            id: job,
            status: ExecJobStatus::Completed,
            ..Default::default()
        }]]);
    (
        event,
        db,
        crate::build_check_context("hosts", "apps.x86_64-linux.update"),
    )
}

#[test]
fn completed_build_or_exec_job_reports_exactly_one_success_commit_status_for_any_requested_event() {
    use gradient_entity::build::BuildStatus;
    use gradient_entity::exec_job::ExecJobStatus;
    use gradient_types::TaskId;

    run(async {
        let task = TaskId::now_v7();
        let build_cases = [
            BuildStatus::Created,
            BuildStatus::Queued,
            BuildStatus::Building,
            BuildStatus::FailedPermanent,
            BuildStatus::Completed,
        ]
        .map(|status| build_event_test_case_with_build_status(status, task));
        let exec_job_cases = [
            ExecJobStatus::Running,
            ExecJobStatus::Completed,
            ExecJobStatus::Failed,
            ExecJobStatus::Aborted,
        ]
        .map(|status| build_exec_job_event_test_case(status, task));

        for (event, db, context) in build_cases.into_iter().chain(exec_job_cases) {
            let check = CommitStatusCheckWithIds::commit_status_check_from_event_name_and_content(
                &event.name(),
                &event.content(),
            )
            .unwrap()
            .unwrap();
            let ctx = make_ctx_with(
                append_task_commit_and_empty_project_result_to_mock_database(db, task),
            );

            let reports = build_ci_reports_from_commit_status_check_rows(&ctx, &check)
                .await
                .unwrap();

            let [report] = reports.as_slice() else {
                panic!("{}: {reports:?}", event.name());
            };
            assert_eq!(report.context, context);
            assert_eq!(report.sha, "abcd");
            assert_eq!(report.status, CiStatus::Success, "{}", event.name());
        }
    });
}

#[test]
fn commit_status_check_with_deleted_evaluation_row_has_no_report_and_settles_delivery() {
    use gradient_types::{EvaluationId, MEvaluation};

    run(async {
        let ctx = make_ctx_with(mock_db().append_query_results([Vec::<MEvaluation>::new()]));
        let check = CommitStatusCheckWithIds::Evaluation {
            evaluation: EvaluationId::now_v7(),
        };

        let reports = build_ci_reports_from_commit_status_check_rows(&ctx, &check)
            .await
            .unwrap();

        assert!(reports.is_empty(), "{reports:?}");
    });
}

#[test]
fn evaluation_check_reports_success_when_no_attribute_failed_and_failure_with_failed_attribute_build_check()
 {
    use gradient_entity::evaluation::EvaluationStatus;
    use gradient_entity::evaluation_message::MessageLevel;
    use gradient_types::{EvaluationId, MEvaluation, MEvaluationMessage, TaskId};

    run(async {
        for attribute_failed in [false, true] {
            let evaluation_id = EvaluationId::now_v7();
            let task = TaskId::now_v7();
            let db = mock_db().append_query_results([vec![MEvaluation {
                id: evaluation_id,
                task: Some(task),
                status: EvaluationStatus::Building,
                building_started_at: Some(gradient_types::now()),
                ..Default::default()
            }]]);
            let messages: Vec<MEvaluationMessage> = attribute_failed
                .then(|| MEvaluationMessage {
                    evaluation: evaluation_id,
                    level: MessageLevel::Error,
                    source: Some("nix-eval:nixosConfigurations.c".into()),
                    message: "failed to evaluate 'nixosConfigurations.c': boom".into(),
                    ..Default::default()
                })
                .into_iter()
                .collect();
            let ctx = make_ctx_with(
                append_task_commit_and_empty_project_result_to_mock_database(db, task)
                    .append_query_results([messages]),
            );
            let check = CommitStatusCheckWithIds::Evaluation {
                evaluation: evaluation_id,
            };

            let reports = build_ci_reports_from_commit_status_check_rows(&ctx, &check)
                .await
                .unwrap();

            let checks: Vec<(&str, &CiStatus)> = reports
                .iter()
                .map(|r| (r.context.as_str(), &r.status))
                .collect();
            let expected = if attribute_failed {
                vec![
                    ("gradient/hosts: Evaluation", &CiStatus::Failure),
                    (
                        "gradient/hosts: Build nixosConfigurations.c",
                        &CiStatus::Failure,
                    ),
                ]
            } else {
                vec![("gradient/hosts: Evaluation", &CiStatus::Success)]
            };
            assert_eq!(checks, expected);
        }
    });
}

#[test]
fn build_event_for_only_evaluate_task_posts_no_commit_status_for_regular_entry_point_and_build_check_for_import_from_derivation_entry_point()
 {
    use gradient_entity::build::BuildStatus;
    use gradient_types::{
        BuildJobId, DerivationBuildId, DerivationId, EvaluationId, MBuildJob, MDerivation,
        MDerivationBuild, MEntryPoint, MEvaluation, TaskId,
    };

    run(async {
        for ifd in [false, true] {
            let evaluation_id = EvaluationId::now_v7();
            let task = TaskId::now_v7();
            let build_job = MBuildJob {
                id: BuildJobId::now_v7(),
                evaluation: evaluation_id,
                derivation: DerivationId::now_v7(),
                derivation_build: DerivationBuildId::now_v7(),
                ..Default::default()
            };
            let db = mock_db()
                .append_query_results([vec![MEvaluation {
                    id: evaluation_id,
                    task: Some(task),
                    only_evaluate: true,
                    ..Default::default()
                }]])
                .append_query_results([vec![build_job.clone()]])
                .append_query_results([vec![MDerivation {
                    id: build_job.derivation,
                    ifd,
                    ..Default::default()
                }]])
                .append_query_results([vec![MDerivationBuild {
                    id: build_job.derivation_build,
                    status: BuildStatus::Created,
                    ..Default::default()
                }]])
                .append_query_results([vec![MEntryPoint {
                    evaluation: evaluation_id,
                    derivation: build_job.derivation,
                    eval: "nixosConfigurations.a.config.system.build.toplevel".into(),
                    ..Default::default()
                }]]);
            let ctx = make_ctx_with(
                append_task_commit_and_empty_project_result_to_mock_database(db, task),
            );
            let check = CommitStatusCheckWithIds::Build {
                evaluation: evaluation_id,
                build_job: build_job.id,
            };

            let reports = build_ci_reports_from_commit_status_check_rows(&ctx, &check)
                .await
                .unwrap();

            assert_eq!(reports.len(), usize::from(ifd), "{reports:?}");
            assert!(reports.iter().all(|r| r.status == CiStatus::Pending));
        }
    });
}

#[test]
fn transient_derivation_build_failure_yields_no_commit_status_report() {
    use gradient_entity::build::BuildStatus;
    use gradient_types::{
        BuildJobId, DerivationBuildId, EvaluationId, MBuildJob, MDerivationBuild, MEvaluation,
        TaskId,
    };

    run(async {
        let evaluation_id = EvaluationId::now_v7();
        let build_job = MBuildJob {
            id: BuildJobId::now_v7(),
            evaluation: evaluation_id,
            derivation_build: DerivationBuildId::now_v7(),
            ..Default::default()
        };
        let ctx = make_ctx_with(
            mock_db()
                .append_query_results([vec![MEvaluation {
                    id: evaluation_id,
                    task: Some(TaskId::now_v7()),
                    ..Default::default()
                }]])
                .append_query_results([vec![build_job.clone()]])
                .append_query_results([vec![MDerivationBuild {
                    id: build_job.derivation_build,
                    status: BuildStatus::FailedTransient,
                    ..Default::default()
                }]]),
        );
        let check = CommitStatusCheckWithIds::Build {
            evaluation: evaluation_id,
            build_job: build_job.id,
        };

        let reports = build_ci_reports_from_commit_status_check_rows(&ctx, &check)
            .await
            .unwrap();

        assert!(reports.is_empty(), "{reports:?}");
    });
}

#[test]
fn an_input_update_reaches_open_pr() {
    let actions = vec![
        open_pr(gradient_types::VerifyGate::Build),
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
fn a_normal_run_never_reaches_open_pr() {
    let actions = vec![
        open_pr(gradient_types::VerifyGate::Build),
        action_with(ActionType::SendMail, vec!["evaluation.completed"]),
    ];
    let payload = json!({"evaluation_kind": "normal"});

    let matched = matching_actions(actions, "evaluation.completed", &payload);

    assert_eq!(
        matched.iter().map(|a| a.action_type).collect::<Vec<_>>(),
        vec![ActionType::SendMail],
    );
}

#[test]
fn an_action_that_does_not_subscribe_is_not_matched() {
    let actions = vec![action_with(ActionType::SendMail, vec!["build.failed"])];

    assert!(matching_actions(actions, "build.completed", &json!({})).is_empty());
}

fn exec_tree() -> gradient_types::MTaskAction {
    use gradient_types::{ActionConfig, GitHostConnectionId, PatchGeneratorKind};

    let mut a = action_with(ActionType::OpenPr, vec![]);
    a.config = serde_json::to_value(ActionConfig::OpenPr {
        git_host_connection_id: GitHostConnectionId::now_v7(),
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
    use gradient_types::{ExecJobId, GitHostConnectionId, TaskActionId, TaskId};

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
                GitHostConnectionId::now_v7(),
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
fn a_failed_connectivity_test_lands_in_the_delivery_history() {
    use gradient_fixtures::mock::raw_statements;

    run(async {
        let db =
            mock_db().append_query_results([vec![gradient_types::MTaskActionDelivery::default()]]);
        let ctx = make_ctx_with(db);
        let action = action_with(ActionType::OpenPr, vec![]);

        let result = super::test_git_host_action(&ctx, &action, "https://example.com/a/b").await;

        assert!(result.is_err());
        let log = raw_statements(ctx.db.worker_db.into_transaction_log());
        let insert = log
            .iter()
            .find(|s| s.sql.starts_with(r#"INSERT INTO "task_action_delivery""#))
            .expect("the attempt is recorded");
        let values = &insert.values.as_ref().unwrap().0;
        assert!(values.contains(&sea_orm::Value::String(Some("test.connectivity".into()))));
        assert!(values.contains(&sea_orm::Value::Bool(Some(false))));
    });
}

#[test]
fn a_failed_delivery_does_not_move_the_last_fired_time() {
    use gradient_fixtures::mock::{exec_ok, raw_statements};
    use gradient_types::actions::ActionConfig;

    run(async {
        let db = mock_db()
            .append_query_results([vec![gradient_types::MTaskActionDelivery::default()]])
            .append_exec_results([exec_ok()]);
        let ctx = make_ctx_with(db);
        let action = gradient_types::MTaskAction {
            config: serde_json::to_value(ActionConfig::SendWebRequest {
                url: "http://127.0.0.1/hook".into(),
                token: None,
            })
            .unwrap(),
            ..action_with(ActionType::SendWebRequest, vec![])
        };

        let result = super::execute_action(&ctx, action, "evaluation.failed", json!({})).await;

        assert!(result.is_err());
        let log = raw_statements(ctx.db.worker_db.into_transaction_log());
        assert!(
            !log.iter().any(|s| s.sql.starts_with("UPDATE task_action")),
            "{log:?}"
        );
    });
}
