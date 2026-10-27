/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::ops::RangeInclusive;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use gradient_db::{DbContext, MissingRow};
use gradient_entity::StorePathError;
use gradient_entity::build::BuildStatus;
use gradient_entity::derivation;
use gradient_entity::entry_point::{License, Licenses};
use gradient_entity::ids::{DispatchedJobId, EvaluationId};
use gradient_fixtures::shape::shape;
use gradient_wire::types::{self, DiscoveredDerivation};
use sea_orm::ConnectionTrait;

use super::{
    A, A_OUT, B, B_OUT, C, C_OUT, Case, D, D_OUT, SOURCE, Z, Z_OUT, batch, chain, drv, nar,
    with_attr, with_output, with_references,
};
use crate::messages::{NarCommit, Transition};
use crate::test_ctx::{Batch, BatchReport};
use crate::writer::{GRAPH_TX_ATTEMPTS, GRAPH_TX_BUDGET};

const NOTE_TRANSACTIONS: &str = "
    CREATE TABLE written_in (source text, top_level text);
    CREATE FUNCTION note_transaction() RETURNS trigger LANGUAGE plpgsql AS $$
    BEGIN
        INSERT INTO written_in VALUES (TG_TABLE_NAME, pg_current_xact_id()::text);
        RETURN NULL;
    END $$;
    CREATE TRIGGER note_derivation AFTER INSERT ON derivation
        FOR EACH ROW EXECUTE FUNCTION note_transaction();
    CREATE TRIGGER note_cached_path AFTER INSERT ON cached_path
        FOR EACH ROW EXECUTE FUNCTION note_transaction();
";

#[derive(Clone, Copy, PartialEq)]
enum Injection {
    Returned,
    Swallowed,
}

struct Injected<T> {
    result: anyhow::Result<T>,
    attempts: u32,
}

// Both futures send a message at the first poll of the single test thread.
// Both messages wait in the mailbox ahead of the first flush.
async fn together<F, S>(first: impl Future<Output = F>, second: impl Future<Output = S>) -> (F, S) {
    tokio::join!(first, second)
}

async fn inject_error<T, F, Fut>(
    case: &Case,
    sqlstate: &str,
    injection: Injection,
    on_attempts: RangeInclusive<u32>,
    work: F,
) -> Injected<T>
where
    F: Fn(DbContext) -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    let raise =
        &format!("DO $$ BEGIN RAISE EXCEPTION 'injected' USING ERRCODE = '{sqlstate}'; END $$");
    let attempts = &AtomicU32::new(0);
    let (work, on_attempts) = (&work, &on_attempts);
    let budget_ms = u64::try_from(GRAPH_TX_BUDGET.as_millis()).expect("the budget fits");
    let result = case
        .with_budget_ms(budget_ms, move |scoped| async move {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
            let done = work(scoped.clone()).await?;
            if on_attempts.contains(&attempt) {
                let raised = scoped.worker_db.execute_unprepared(raise).await;
                if injection == Injection::Returned {
                    raised?;
                }
            }

            Ok(done)
        })
        .await;

    Injected {
        result,
        attempts: attempts.load(Ordering::SeqCst),
    }
}

async fn walk_with_error(
    case: &Case,
    evaluation: EvaluationId,
    derivation: DiscoveredDerivation,
    sqlstate: &str,
    on_attempts: RangeInclusive<u32>,
) -> Injected<BatchReport> {
    let batch = &case.tasked(evaluation, vec![derivation]).await;
    inject_error(
        case,
        sqlstate,
        Injection::Returned,
        on_attempts,
        |scoped| async move { Case::walk_in(&scoped, batch).await },
    )
    .await
}

fn entry_point(path: &str, output: &str, attr: &str) -> DiscoveredDerivation {
    with_attr(with_output(drv(path, &[]), output), attr)
}

// Savepoints stamp xmin with a subtransaction.
// The triggers note the top-level transaction of every new row instead.
async fn transactions_writing(case: &Case, table: &str) -> Vec<String> {
    let written = format!("SELECT top_level FROM written_in WHERE source = '{table}'");
    case.db.texts(&written).await
}

#[tokio::test]
async fn m01_a_batch_for_an_unknown_evaluation_fails_only_itself_inside_a_shared_flush() {
    let (e1, unknown) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = Case::start(shape().evaluation(e1)).await;
    let gv = case.gv(e1).await;
    let of_e1 = case.tasked(e1, vec![entry_point(A, A_OUT, "pkg.a")]).await;
    let of_nobody = Batch {
        derivations: vec![entry_point(Z, Z_OUT, "pkg.z")],
        task: of_e1.task,
        ..batch(unknown)
    };

    let (first, second) = together(case.walk_result(of_e1), case.walk_result(of_nobody)).await;

    assert!(first.is_ok(), "after 1: first result ok: {first:?}");
    let error = second.expect_err("after 1: second result error");
    assert_eq!(
        error.downcast_ref::<MissingRow>(),
        Some(&MissingRow::Evaluation { id: unknown }),
        "after 1: second result: {error:#}"
    );
    let (a, z) = (case.find_derivation(A).await, case.find_derivation(Z).await);
    assert!(a.is_some(), "after 1: derivation(A) present");
    assert_eq!(z, None, "after 1: no derivation(Z)");
    assert!(case.gv(e1).await > gv, "after 1: gv(E1) rose");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn m02_a_bad_dependency_path_fails_only_its_batch_in_a_shared_flush() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = Case::start(shape().evaluation(e1).evaluation(e2)).await;
    let (gv1, gv2) = (case.gv(e1).await, case.gv(e2).await);
    let above_bad = with_attr(with_output(drv(A, &["bad"]), A_OUT), "pkg.a");
    let of_e1 = case.tasked(e1, vec![above_bad]).await;
    let of_e2 = case.tasked(e2, vec![entry_point(C, C_OUT, "pkg.c")]).await;

    let (first, second) = together(case.walk_result(of_e1), case.walk_result(of_e2)).await;

    assert!(first.is_err(), "after 1: first result error: {first:?}");
    assert!(second.is_ok(), "after 1: second result ok: {second:?}");
    let (a, c) = (case.find_derivation(A).await, case.find_derivation(C).await);
    assert_eq!(a, None, "after 1: no derivation(A)");
    assert!(c.is_some(), "after 1: derivation(C) present");
    assert_eq!(case.gv(e1).await, gv1, "after 1: gv(E1) unchanged");
    assert_eq!(case.gv(e2).await, gv2 + 1, "after 1: gv(E2) +1");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn m03_a_malformed_nar_among_queued_commits_fails_alone_and_the_good_paths_are_committed() {
    let e1 = EvaluationId::now_v7();
    let seed = chain(e1).derivation(B).build_status(BuildStatus::Completed);
    let case = Case::start_with_unstored_outputs(seed, 1).await;
    let malformed = "/nix/store/zz-not-nix32-x";
    let of_b = with_references(nar(B_OUT, "b"), &[(B_OUT, "b")]);
    let of_malformed = NarCommit {
        store_path: malformed.into(),
        ..nar(SOURCE, "source")
    };
    let of_source = nar(SOURCE, "source");

    let (first, (second, third)) = together(
        case.commit_nar_result(of_b),
        together(
            case.commit_nar_result(of_malformed),
            case.commit_nar_result(of_source),
        ),
    )
    .await;

    assert!(first.is_ok(), "after 1: first result ok: {first:?}");
    assert!(third.is_ok(), "after 1: third result ok: {third:?}");
    let error = second.expect_err("after 1: second result error");
    assert_eq!(
        error.downcast_ref::<StorePathError>(),
        Some(&StorePathError::Malformed(malformed.into())),
        "after 1: second result: {error:#}"
    );
    let stored = case.find_cached_path(B_OUT).await;
    assert!(stored.is_some(), "after 1: cached_path(B_OUT) present");
    let stored = case.find_cached_path(SOURCE).await;
    assert!(stored.is_some(), "after 1: cached_path(SRC) present");
    assert!(case.sb(B).await.fetchable, "after 1: sb(B).fetchable");
    let status = case.sb(A).await.status;
    assert_eq!(status, BuildStatus::Queued, "after 1: sb(A).status");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn m04_a_batch_and_a_commit_queued_together_see_each_other_after_the_flush() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;
    case.sql(NOTE_TRANSACTIONS).await;
    let walk = case.tasked(e1, vec![entry_point(A, A_OUT, "pkg.a")]).await;
    let commit = with_references(nar(A_OUT, "a"), &[(A_OUT, "a")]);

    let (walked, committed) =
        together(case.walk_result(walk), case.commit_nar_result(commit)).await;

    walked.expect("after 1: the batch is written");
    committed.expect("after 1: the NAR is committed");
    let stored = case.output_stored(A, "out").await;
    assert!(stored, "after 1: cached_path(output(A,out).hash) stored");
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.missing_runtime_deps, a.fetchable),
        (BuildStatus::Created, 0, false),
        "after 1: sb(A)"
    );
    let of_walk = transactions_writing(&case, "derivation").await;
    let of_commit = transactions_writing(&case, "cached_path").await;
    assert_eq!(of_walk.len(), 1, "after 1: the batch wrote A: {of_walk:?}");
    assert_eq!(
        of_walk, of_commit,
        "after 1: the batch and the commit share a transaction"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn m05_a_transaction_past_its_budget_is_rolled_back_completely() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;
    let gv = case.gv(e1).await;
    let walk = &case.tasked(e1, vec![entry_point(A, A_OUT, "pkg.a")]).await;

    let slow = case
        .with_budget_ms(50, |scoped| async move {
            Case::walk_in(&scoped, walk).await?;
            tokio::time::sleep(Duration::from_millis(500)).await;
            Ok(())
        })
        .await;

    let error = slow.expect_err("after 1: the transaction is past its budget");
    assert!(
        error.to_string().contains("graph transaction exceeded"),
        "after 1: {error:#}"
    );
    let derivations = case.db.rows::<derivation::Entity>().await;
    assert!(derivations.is_empty(), "after 1: no derivation");
    assert_eq!(case.gv(e1).await, gv, "after 1: gv(E1) unchanged");
    case.zero_drift("step 1").await;

    case.walk(walk.clone()).await;

    let a = case.find_derivation(A).await;
    assert!(a.is_some(), "after 2: derivation(A) present");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn m06_a_deadlock_or_serialization_error_retries_the_transaction_a_unique_violation_not() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;
    let gv = case.gv(e1).await;

    let a = entry_point(A, A_OUT, "pkg.a");
    let deadlock = walk_with_error(&case, e1, a, "40P01", 1..=1).await;

    deadlock.result.expect("after 1: ok");
    assert_eq!(deadlock.attempts, 2, "after 1: the second attempt commits");
    let a = case.find_derivation(A).await;
    assert!(a.is_some(), "after 1: derivation(A) present");
    assert_eq!(case.gv(e1).await, gv + 1, "after 1: gv(E1) +1 exactly");
    case.zero_drift("step 1").await;

    let c = entry_point(C, C_OUT, "pkg.c");
    let serialization = walk_with_error(&case, e1, c, "40001", 1..=GRAPH_TX_ATTEMPTS).await;

    assert!(serialization.result.is_err(), "after 2: error");
    assert_eq!(
        serialization.attempts, GRAPH_TX_ATTEMPTS,
        "after 2: error once every attempt failed"
    );
    let c = case.find_derivation(C).await;
    assert_eq!(c, None, "after 2: no derivation(C)");
    case.zero_drift("step 2").await;

    let d = entry_point(D, D_OUT, "pkg.d");
    let unique = walk_with_error(&case, e1, d, "23505", 1..=1).await;

    assert!(unique.result.is_err(), "after 3: error");
    assert_eq!(unique.attempts, 1, "after 3: error after 1 attempt");
    let d = case.find_derivation(D).await;
    assert_eq!(d, None, "after 3: no derivation(D)");
    case.zero_drift("step 3").await;
    case.finish().await;
}

#[tokio::test]
async fn m07_a_transaction_aborted_by_a_swallowed_error_is_rolled_back_not_committed() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(chain(e1)).await;
    let shared_build = case.sb(B).await.id;

    let swallowed = inject_error(
        &case,
        "22P02",
        Injection::Swallowed,
        1..=GRAPH_TX_ATTEMPTS,
        |scoped| async move {
            let started = Transition::BuildStarted {
                shared_build,
                dispatched_job: DispatchedJobId::now_v7(),
                substitute: false,
                build_context: serde_json::json!({}),
            };
            crate::transition::apply(&scoped, started).await
        },
    )
    .await;

    let error = swallowed.result.expect_err("after 1: result error");
    assert!(
        error.to_string().contains("aborted before commit"),
        "after 1: {error:#}"
    );
    assert_eq!(
        swallowed.attempts, GRAPH_TX_ATTEMPTS,
        "after 1: every attempt is aborted"
    );
    let status = case.sb(B).await.status;
    assert_eq!(status, BuildStatus::Queued, "after 1: sb(B).status");
    let attempts = case.attempts(B).await;
    assert!(
        attempts.is_empty(),
        "after 1: no attempt of B: {attempts:?}"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 2, 0, 1, 0],
        "after 1: counters(E1)"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn m08_a_commit_outside_the_transaction_of_the_writer_is_refused() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(chain(e1)).await;
    let commit = with_references(nar(B_OUT, "b"), &[(B_OUT, "b")]);

    let refused = case.commit_nar_outside_tx(commit).await;

    assert!(refused.is_err(), "after 1: error: {refused:?}");
    let stored = case.find_cached_path(B_OUT).await;
    assert_eq!(stored, None, "after 1: no cached_path(B_OUT)");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn m09_an_entry_point_keeps_the_licenses_of_its_attribute() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;
    let licensed = DiscoveredDerivation {
        licenses: vec![types::License::Spdx { id: "MIT".into() }],
        ..entry_point(A, A_OUT, "pkg.a")
    };

    case.walk(case.tasked(e1, vec![licensed]).await).await;

    let points = case.entry_points(e1).await;
    assert_eq!(
        points.iter().map(|p| &p.licenses).collect::<Vec<_>>(),
        vec![&Licenses(vec![License::Spdx { id: "MIT".into() }])],
        "after 1: licenses of pkg.a"
    );
    case.finish().await;
}
