/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::entry_point;
use gradient_entity::evaluation::{EvaluationKind, EvaluationStatus};
use gradient_entity::ids::EvaluationId;
use gradient_entity::task;
use gradient_fixtures::shape::shape;
use gradient_wire::types::BuildFailureKind;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

use super::{
    A, A_OUT, B, B_OUT, CaseShapes, SchedulerCase, attr_of, drv, eval_key, failure, with_attr,
    with_output,
};

const W1: &str = "w1";

async fn entry_point_attribute_names_for_evaluation(
    case: &SchedulerCase,
    evaluation: EvaluationId,
) -> Vec<String> {
    let mut attrs: Vec<String> = entry_point::Entity::find()
        .filter(entry_point::Column::Evaluation.eq(evaluation))
        .all(&case.scheduler.db.worker_db)
        .await
        .expect("the entry points read")
        .into_iter()
        .map(|ep| ep.eval)
        .collect();
    attrs.sort();
    attrs
}

#[tokio::test]
async fn retry_after_failed_walk_starts_queued_and_records_full_walk_entry_points() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let complete_walk = vec![
        with_attr(with_output(drv(A, &[]), A_OUT), &attr_of(A)),
        with_attr(with_output(drv(B, &[]), B_OUT), &attr_of(B)),
    ];
    let mut complete_walk_attrs = vec![attr_of(A), attr_of(B)];
    complete_walk_attrs.sort();

    let k1 = eval_key(e1);
    let a1 = case.evalrun(W1, e1).await;
    case.advance_evaluation(W1, &k1, EvaluationStatus::EvaluatingDerivation)
        .await;
    case.report_eval_result(W1, &k1, complete_walk[..1].to_vec())
        .await;
    case.fail(
        W1,
        &k1,
        a1,
        failure(BuildFailureKind::Permanent).error("evaluation crashed"),
    )
    .await;
    let failed = case.evaluation(e1).await;
    assert_eq!(
        failed.status,
        EvaluationStatus::Failed,
        "after 1: E1 failed"
    );
    assert_eq!(
        entry_point_attribute_names_for_evaluation(&case, e1).await,
        vec![attr_of(A)],
        "after 1: E1 recorded only some of its entry points"
    );
    case.check("step 1").await;

    let task = case
        .db
        .find::<task::Entity>(case.db.task_of(e1).await)
        .await
        .expect("the task of E1");
    let retry = gradient_ci::trigger_evaluation_retry(
        &case.scheduler.db.worker_db,
        &task,
        &failed,
        gradient_fixtures::rows::user().id,
    )
    .await
    .expect("the retry is created");
    assert_eq!(
        (retry.kind, retry.status),
        (EvaluationKind::Retry, EvaluationStatus::Queued),
        "after 2: retry of the unfinished walk from scratch"
    );
    assert!(
        entry_point_attribute_names_for_evaluation(&case, retry.id)
            .await
            .is_empty(),
        "after 2: a retry doesn't inherit any partial entry points"
    );
    case.check("step 2").await;

    let k2 = eval_key(retry.id);
    let a2 = case.evalrun(W1, retry.id).await;
    case.advance_evaluation(W1, &k2, EvaluationStatus::EvaluatingDerivation)
        .await;
    case.report_eval_result(W1, &k2, complete_walk).await;
    case.complete(W1, &k2, a2, Vec::new(), 1).await;
    assert_eq!(
        case.evaluation(retry.id).await.status,
        EvaluationStatus::Building,
        "after 3: retry build after its own walk"
    );
    assert_eq!(
        entry_point_attribute_names_for_evaluation(&case, retry.id).await,
        complete_walk_attrs,
        "after 3: the retry stored all entry points from a complete walk"
    );
    case.check("step 3").await;

    case.finish().await;
}
