/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::future::Future;
use std::time::Duration;

use gradient_entity::build::BuildStatus;
use gradient_entity::ids::{EvaluationId, ProjectId};
use gradient_entity::{cache_role, evaluation};
use gradient_fixtures::shape::{Shape, shape};
use gradient_pool::session_port::SessionSignal;
use gradient_wire::types::JobKind;
use sea_orm::{EntityTrait, QuerySelect, TransactionTrait};

use super::{SchedulerCase, w_build, w_eval};
use crate::jobs::build_job_key;
use crate::test_jobs::eval_job;

const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a.drv";
const A_OUT: &str = "a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0";
const W: &str = "w1";
const STEP_BUDGET: Duration = Duration::from_secs(5);

fn queued_build(evaluation: EvaluationId) -> Shape {
    shape()
        .evaluation(evaluation)
        .derivation(A)
        .walked()
        .entry_point("pkg.a")
        .job()
        .probed()
        .drv_cached()
        .output(A_OUT)
        .build_status(BuildStatus::Queued)
}

async fn connect_builder(case: &SchedulerCase) {
    case.connect(W, w_build(), &[]).await;
}

async fn admit(case: &SchedulerCase) {
    case.admit_startable_moves().await;
}

async fn offered_and_scored(case: &SchedulerCase) {
    case.offers_full(W).await;
    case.score_all(W).await;
}

async fn key_of_a(case: &SchedulerCase) -> String {
    build_job_key(case.db.shared_build(A).await.id)
}

async fn within_budget<T>(step: &str, future: impl Future<Output = T>) -> T {
    tokio::time::timeout(STEP_BUDGET, future)
        .await
        .unwrap_or_else(|_| panic!("{step} took longer than {STEP_BUDGET:?}"))
}

#[tokio::test]
async fn a_case_finishes_while_an_abort_still_runs() {
    let e = EvaluationId::now_v7();
    let case = SchedulerCase::start(queued_build(e)).await;
    connect_builder(&case).await;
    admit(&case).await;
    offered_and_scored(&case).await;

    let assignment = case.request_job(W, JobKind::Build).await;
    assert!(assignment.is_some(), "W gets the build of A");
    let evaluation = case
        .db
        .find::<evaluation::Entity>(e)
        .await
        .expect("the evaluation row");
    case.scheduler.abort_evaluation(evaluation).await;
    case.settled().await;

    assert_eq!(
        case.db.shared_build(A).await.status,
        BuildStatus::Aborted,
        "sb(A) after the graph half of the abort"
    );
    case.finish().await;
}

#[tokio::test]
async fn an_open_transaction_starves_no_step() {
    let case = SchedulerCase::start(queued_build(EvaluationId::now_v7())).await;
    connect_builder(&case).await;
    let locking = case
        .db
        .connection()
        .begin()
        .await
        .expect("a transaction opens");
    cache_role::Entity::find()
        .lock_exclusive()
        .all(&locking)
        .await
        .expect("the cache roles lock");

    within_budget("the admission", admit(&case)).await;
    within_budget("the offer and the scores", offered_and_scored(&case)).await;
    let assignment = within_budget("the request", case.request_job(W, JobKind::Build)).await;

    assert!(assignment.is_some(), "W gets the build of A");
    locking
        .rollback()
        .await
        .expect("the transaction rolls back");
    case.finish().await;
}

#[tokio::test]
async fn a_seeded_queued_build_is_pending_after_admission() {
    let case = SchedulerCase::start(queued_build(EvaluationId::now_v7())).await;

    admit(&case).await;

    let key = key_of_a(&case).await;
    assert!(
        case.scheduler.pending_job(&key).await.is_some(),
        "{key} pending after the first admission"
    );
    case.check("the admission").await;
    case.finish().await;
}

#[tokio::test]
async fn signals_are_taken_once() {
    let case = SchedulerCase::start(shape()).await;
    case.connect(W, w_eval(), &[]).await;
    let project = ProjectId::now_v7();

    for key in ["eval-1", "eval-2"] {
        case.scheduler
            .enqueue_eval_job(key.into(), eval_job(project))
            .await
            .expect("the job enqueues");
    }

    let first = case.signals(W);
    assert!(
        matches!(
            first.as_slice(),
            [SessionSignal::Offers(_), SessionSignal::Offers(_)]
        ),
        "first call: {first:?}"
    );
    assert_eq!(case.signals(W), vec![], "second call");
    case.finish().await;
}

#[tokio::test]
async fn a_second_instance_keeps_its_own_memory() {
    let case = SchedulerCase::start(queued_build(EvaluationId::now_v7())).await;
    let second = case.second_instance().await;
    let key = key_of_a(&case).await;

    admit(&second).await;
    assert!(
        second.scheduler.pending_job(&key).await.is_none(),
        "{key} on the second instance after its admission"
    );
    assert!(
        case.scheduler.pending_job(&key).await.is_none(),
        "{key} on the first instance before its admission"
    );

    admit(&case).await;
    assert!(
        case.scheduler.pending_job(&key).await.is_some(),
        "{key} on the first instance after its admission"
    );
    assert!(
        second.scheduler.pending_job(&key).await.is_none(),
        "{key} on the second instance after the admission of the first"
    );

    second.finish().await;
    case.finish().await;
}
