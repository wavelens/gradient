/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::evaluation_message;
use gradient_entity::ids::EvaluationId;
use gradient_fixtures::shape::shape;
use gradient_types::runner::{RunnerSpec, runner_image_attr};
use gradient_wire::types::DiscoveredDerivation;

use super::{A, A_OUT, B, B_OUT, Case, drv, with_attr, with_output};

const ATTR: &str = "apps.x86_64-linux.a";

fn entry_point(path: &str, output: &str, attr: &str) -> DiscoveredDerivation {
    with_attr(with_output(drv(path, &[]), output), attr)
}

fn spec() -> RunnerSpec {
    RunnerSpec {
        program: format!("{A_OUT}/bin/a"),
        nodes: 1,
        ..Default::default()
    }
}

fn runner_root() -> DiscoveredDerivation {
    DiscoveredDerivation {
        runner: Some(spec()),
        ..entry_point(A, A_OUT, ATTR)
    }
}

fn image_root() -> DiscoveredDerivation {
    entry_point(B, B_OUT, &runner_image_attr(ATTR))
}

async fn switch_runner_jobs_on(case: &Case) {
    case.sql("UPDATE project SET runner_enabled = true").await;
}

async fn messages(case: &Case, evaluation: EvaluationId) -> Vec<(String, Option<String>)> {
    case.db
        .rows::<evaluation_message::Entity>()
        .await
        .into_iter()
        .filter(|m| m.evaluation == evaluation)
        .map(|m| (m.message, m.source))
        .collect()
}

#[tokio::test]
async fn a_runner_entry_point_keeps_its_spec() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;
    switch_runner_jobs_on(&case).await;

    case.walk(case.tasked(e1, vec![runner_root()]).await).await;

    let points = case.entry_points(e1).await;
    assert_eq!(points.len(), 1, "after 1: one entry point");
    let stored: RunnerSpec =
        serde_json::from_value(points[0].runner.clone().expect("after 1: a runner column"))
            .expect("after 1: the spec decodes");
    assert_eq!(stored, spec(), "after 1: the stored spec");
    case.finish().await;
}

#[tokio::test]
async fn a_project_without_runner_jobs_fails_the_attribute_with_its_message() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;

    case.walk(case.tasked(e1, vec![runner_root()]).await).await;

    assert!(
        case.entry_points(e1).await.is_empty(),
        "after 1: no entry point"
    );
    assert_eq!(
        messages(&case, e1).await,
        vec![(
            "runner jobs are disabled for this project".to_owned(),
            Some(format!("nix-eval:{ATTR}"))
        )],
        "after 1: the refusal"
    );
    case.finish().await;
}

#[tokio::test]
async fn an_instance_without_runner_jobs_fails_the_attribute_with_its_message() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start_without_runner_jobs(shape().evaluation(e1)).await;
    switch_runner_jobs_on(&case).await;

    case.walk(case.tasked(e1, vec![runner_root()]).await).await;

    assert!(
        case.entry_points(e1).await.is_empty(),
        "after 1: no entry point"
    );
    assert_eq!(
        messages(&case, e1).await,
        vec![(
            "runner jobs are disabled on this instance".to_owned(),
            Some(format!("nix-eval:{ATTR}"))
        )],
        "after 1: the refusal"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_refused_runner_inserts_neither_its_program_nor_its_image() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;

    case.walk(case.tasked(e1, vec![runner_root(), image_root()]).await)
        .await;

    assert!(
        case.entry_points(e1).await.is_empty(),
        "after 1: no entry point"
    );
    assert!(
        !case.sb(A).await.wanted,
        "after 1: the program is not wanted"
    );
    assert!(!case.sb(B).await.wanted, "after 1: the image is not wanted");
    case.finish().await;
}

#[tokio::test]
async fn a_refused_runner_skips_its_image_in_a_later_batch() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;

    case.walk(case.tasked(e1, vec![runner_root()]).await).await;
    case.walk(case.tasked(e1, vec![image_root()]).await).await;

    assert!(
        case.entry_points(e1).await.is_empty(),
        "after 2: no entry point"
    );
    assert!(!case.sb(B).await.wanted, "after 2: the image is not wanted");
    case.finish().await;
}
