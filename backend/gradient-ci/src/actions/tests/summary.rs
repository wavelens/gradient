/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::fixtures::{make_ctx, make_ctx_with, run};
use crate::actions::summary::EventSummary;
use gradient_types::{EvaluationId, MCommit, MEvaluation, MProject, MTask, ProjectId, TaskId};
use sea_orm::{DatabaseBackend, MockDatabase};
use serde_json::json;
use uuid::Uuid;

fn build_failed() -> EventSummary {
    EventSummary {
        event: "build.failed".into(),
        id: "b-1".into(),
        status: "failed".into(),
        time: "2026-10-02T12:00:00Z".into(),
        project: Some("web".into()),
        task: Some("app".into()),
        commit: Some("3f9c2ab".into()),
        derivation: Some("hello-2.12.1".into()),
        link: Some("https://ci.example/project/web/log/e-1".into()),
    }
}

#[test]
fn subject_fills_placeholders_from_names() {
    let s = build_failed();
    assert_eq!(s.subject(None), "[Gradient] build.failed: app");
    assert_eq!(
        s.subject(Some("{project}/{task} {status} {id}")),
        "web/app failed b-1"
    );
}

#[test]
fn mail_body_keeps_event_and_time() {
    let body = build_failed().mail_body();
    assert!(body.starts_with("web/app: hello-2.12.1 failed on 3f9c2ab\n"));
    assert!(body.contains("Event: build.failed\n"));
    assert!(body.contains("Time: 2026-10-02T12:00:00Z\n"));
    assert!(body.contains("Link: https://ci.example/project/web/log/e-1\n"));
}

#[test]
fn test_fire_payload_reads_names_from_content() {
    run(async {
        let envelope = json!({
            "event": "evaluation.completed",
            "at": "2026-10-02T12:00:00Z",
            "content": {
                "project": "web", "task": "app",
                "sha": "0000000000000000000000000000000000000000",
                "link": "https://gradient.example/tasks/web/app",
            },
        });
        let s = EventSummary::resolve(&make_ctx(), "evaluation.completed", &envelope)
            .await
            .unwrap();
        assert!(s.mail_body().starts_with("web/app completed on 0000000\n"));
        assert!(
            s.mail_body()
                .contains("Link: https://gradient.example/tasks/web/app\n")
        );
    });
}

#[test]
fn status_spells_out_the_event_suffix() {
    run(async {
        let envelope = json!({ "at": "t", "content": {} });
        let s = EventSummary::resolve(&make_ctx(), "evaluation.action_required", &envelope)
            .await
            .unwrap();
        assert_eq!(s.status, "action required");
        assert!(
            s.mail_body()
                .starts_with("evaluation.action_required action required\n")
        );
    });
}

#[test]
fn build_event_resolves_names_commit_and_link_from_the_evaluation() {
    run(async {
        let evaluation_id = EvaluationId::now_v7();
        let task = TaskId::now_v7();
        let project = ProjectId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![MEvaluation {
                id: evaluation_id,
                task: Some(task),
                ..Default::default()
            }]])
            .append_query_results([vec![MTask {
                id: task,
                project,
                name: "app".into(),
                ..Default::default()
            }]])
            .append_query_results([vec![MCommit {
                hash: vec![0x3f, 0x9c, 0x2a, 0xb1],
                ..Default::default()
            }]])
            .append_query_results([vec![MProject {
                id: project,
                name: "web".into(),
                ..Default::default()
            }]]);
        let ctx = make_ctx_with(db);
        let envelope = json!({
            "at": "t",
            "content": {
                "build_id": "b-1",
                "evaluation_id": evaluation_id.to_string(),
                "derivation_path": "/nix/store/0c3xq5ka1w7zwh4v4qg0j3jxj7y5l6z2-hello-2.12.1.drv",
            },
        });
        let s = EventSummary::resolve(&ctx, "build.failed", &envelope)
            .await
            .unwrap();
        assert_eq!(s.id, "b-1");
        assert!(
            s.mail_body()
                .starts_with("web/app: hello-2.12.1 failed on 3f9c2ab\n")
        );
        assert_eq!(
            s.link,
            Some(format!(
                "{}/project/web/log/{evaluation_id}",
                ctx.db.config.server.frontend_url
            ))
        );
    });
}

#[test]
fn collected_evaluation_still_yields_a_message() {
    run(async {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MEvaluation>::new()]);
        let envelope = json!({
            "at": "t",
            "content": { "evaluation_id": Uuid::now_v7().to_string() },
        });
        let s = EventSummary::resolve(&make_ctx_with(db), "evaluation.failed", &envelope)
            .await
            .unwrap();
        assert!(s.mail_body().starts_with("evaluation.failed failed\n"));
        assert_eq!(s.link, None);
    });
}

#[test]
fn plain_names_scope_derivation_status_and_commit() {
    assert_eq!(
        build_failed().plain(),
        "web/app: hello-2.12.1 failed on 3f9c2ab\nhttps://ci.example/project/web/log/e-1"
    );
}

#[test]
fn html_links_and_escapes() {
    let mut s = build_failed();
    s.task = Some("<b>&app".into());
    assert_eq!(
        s.html(),
        "web/&lt;b&gt;&amp;app: hello-2.12.1 failed on 3f9c2ab<br>\
         <a href=\"https://ci.example/project/web/log/e-1\">View evaluation</a>"
    );
}

#[test]
fn slack_escapes_and_links() {
    let mut s = build_failed();
    s.task = Some("a<b>&c".into());
    assert_eq!(
        s.slack(),
        "web/a&lt;b&gt;&amp;c: hello-2.12.1 failed on 3f9c2ab \
         <https://ci.example/project/web/log/e-1|View evaluation>"
    );
}

#[test]
fn plain_text_keeps_markup_characters() {
    let mut s = build_failed();
    s.task = Some("a<b>&c".into());
    s.link = None;
    assert_eq!(s.plain(), "web/a<b>&c: hello-2.12.1 failed on 3f9c2ab");
}
