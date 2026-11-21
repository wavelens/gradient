/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeSet;
use std::future::poll_fn;
use std::ops::RangeInclusive;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::{Duration, Instant};

use gradient_db::connection::find_sessions_with_transaction_older_than;
use gradient_db::graph::can_start::{SWEEP_CHUNKS_PER_TRANSACTION, SWEEP_LOCK_CHUNK};
use gradient_db::{DbContext, MissingRow};
use gradient_entity::StorePathError;
use gradient_entity::build::BuildStatus;
use gradient_entity::entry_point::{License, Licenses};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::{
    CacheId, DerivationBuildId, DerivationId, DispatchedJobId, EvaluationId,
};
use gradient_entity::{derivation, derivation_build};
use gradient_fixtures::shape::shape;
use gradient_fixtures::test_date;
use gradient_util::shutdown::Shutdown;
use gradient_wire::types::{self, DiscoveredDerivation};
use sea_orm::{
    ConnectionTrait, DatabaseTransaction, EntityTrait, IntoActiveModel, TransactionTrait,
};

use super::signing::add_waiting_build_that_depends_on_completed_build;
use super::{
    A, A_OUT, B, B_OUT, C, C_OUT, Case, D, D_OUT, SOURCE, Z, Z_OUT, batch, chain, drv, nar,
    walked_a_above_a_stub, with_attr, with_output, with_references,
};
use crate::messages::{NarCommit, Transition};
use crate::test_ctx::{Batch, BatchReport};
use crate::writer::{
    DEFAULT_GRAPH_WRITER_MESSAGE_TRANSACTION_LIMITS, GRAPH_TX_ATTEMPTS,
    GraphWriterTransactionLimits, postgres_sqlstate_from_error_chain,
};

const NOTE_TRANSACTIONS: &str = "
    CREATE TABLE written_in (source text, top_level text);
    CREATE FUNCTION note_transaction() RETURNS trigger LANGUAGE plpgsql AS $$
    BEGIN
        INSERT INTO written_in VALUES (coalesce(TG_ARGV[0], TG_TABLE_NAME), pg_current_xact_id()::text);
        RETURN NULL;
    END $$;
    CREATE TRIGGER note_derivation AFTER INSERT ON derivation
        FOR EACH ROW EXECUTE FUNCTION note_transaction();
    CREATE TRIGGER note_cached_path AFTER INSERT ON cached_path
        FOR EACH ROW EXECUTE FUNCTION note_transaction();
";

const DERIVATION_UPDATE_TRANSACTION_ID_TRIGGER_SQL: &str = "
    CREATE TRIGGER note_top_level_transaction_on_unwalked_inputs_update
        AFTER UPDATE OF unwalked_inputs ON derivation
        FOR EACH ROW WHEN (OLD.unwalked_inputs <> NEW.unwalked_inputs)
        EXECUTE FUNCTION note_transaction('unwalked_inputs');
    CREATE TRIGGER note_wanted_change_transaction_id
        AFTER UPDATE OF wanted ON derivation_build
        FOR EACH ROW WHEN (OLD.wanted <> NEW.wanted)
        EXECUTE FUNCTION note_transaction('wanted');
    CREATE TRIGGER note_blocking_deps_change_transaction_id
        AFTER UPDATE OF blocking_deps ON derivation_build
        FOR EACH ROW WHEN (OLD.blocking_deps <> NEW.blocking_deps)
        EXECUTE FUNCTION note_transaction('blocking_deps');
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

async fn poll_once_and_check_still_pending<T>(
    mut future: Pin<&mut impl Future<Output = T>>,
) -> bool {
    poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_pending())).await
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
    let result = case
        .run_in_transaction_with_limits(
            DEFAULT_GRAPH_WRITER_MESSAGE_TRANSACTION_LIMITS,
            move |scoped| async move {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                let done = work(scoped.clone()).await?;
                if on_attempts.contains(&attempt) {
                    let raised = scoped.worker_db.execute_unprepared(raise).await;
                    if injection == Injection::Returned {
                        raised?;
                    }
                }

                Ok(done)
            },
        )
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

async fn open_second_transaction_and_lock_derivation_build_row(
    case: &Case,
    path: &str,
) -> DatabaseTransaction {
    let derivation = case.db.id_of(path).await;
    let lock =
        format!("SELECT 1 FROM derivation_build WHERE derivation = '{derivation}' FOR UPDATE");
    let holder = case
        .ctx
        .worker_db
        .begin()
        .await
        .expect("open a second transaction");
    let locked = holder
        .execute_unprepared(&lock)
        .await
        .map(|done| done.rows_affected());
    assert_eq!(
        locked.as_ref().ok(),
        Some(&1),
        "one lock on one shared build row for {path}"
    );
    holder
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

async fn distinct_top_level_transaction_ids_that_changed_counter_column(
    case: &Case,
    counter: &str,
) -> BTreeSet<String> {
    transactions_writing(case, counter)
        .await
        .into_iter()
        .collect()
}

async fn insert_many_created_shared_builds_with_wrong_blocking_deps(case: &Case, count: usize) {
    let connection = case.db.connection();
    let derivations: Vec<derivation::Model> = (0..count)
        .map(|i| derivation::Model {
            id: DerivationId::now_v7(),
            hash: format!("{i:032}"),
            name: "bulk".into(),
            architecture: "x86_64-linux".into(),
            walked: true,
            created_at: test_date(),
            ..Default::default()
        })
        .collect();
    for chunk in derivations.chunks(SWEEP_LOCK_CHUNK) {
        let shared_builds = chunk.iter().map(|d| derivation_build::Model {
            id: DerivationBuildId::now_v7(),
            derivation: d.id,
            status: BuildStatus::Created,
            blocking_deps: 1,
            created_at: test_date(),
            updated_at: test_date(),
            ..Default::default()
        });
        derivation::Entity::insert_many(
            chunk
                .iter()
                .cloned()
                .map(IntoActiveModel::into_active_model),
        )
        .exec_without_returning(&connection)
        .await
        .expect("insert derivation");
        derivation_build::Entity::insert_many(
            shared_builds.map(IntoActiveModel::into_active_model),
        )
        .exec_without_returning(&connection)
        .await
        .expect("insert shared builds");
    }
}

#[tokio::test]
async fn m01_a_batch_for_an_unknown_evaluation_fails_only_itself_inside_a_shared_flush() {
    let (e1, unknown) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = Case::start(shape().evaluation(e1)).await;
    let gv = case.gv(e1).await;
    let of_e1 = case.tasked(e1, vec![entry_point(A, A_OUT, "pkg.a")]).await;
    let of_nobody = Batch {
        derivations: vec![
            crate::messages::EvaluationBatchDerivation::test_batch_entry_from_discovered_derivation(
                entry_point(Z, Z_OUT, "pkg.z"),
            ),
        ],
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
    let commit = NarCommit {
        producer: case.make_worker_producer_for_evaluation_project(e1).await,
        ..with_references(nar(A_OUT, "a"), &[(A_OUT, "a")])
    };
    crate::testing::write_commit_compressed_object_to_object_storage(&case.ctx, &commit).await;

    let (walked, committed) = together(case.walk_result(walk), case.graph.commit_nar(commit)).await;

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
    let ([walk], [commit]) = (of_walk.as_slice(), of_commit.as_slice()) else {
        panic!(
            "after 1: {of_walk:?} {of_commit:?} come from one transaction for the batch and one for the commit."
        );
    };
    let transaction_order = |id: &String| id.parse::<u64>().expect("Transaction ID is a number");
    assert!(
        transaction_order(walk) < transaction_order(commit),
        "after 1: batch commit with a transaction older than the NAR: {walk} {commit}"
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

    let graph_writer_transaction_limits_with_50_ms_total_time_budget =
        GraphWriterTransactionLimits {
            transaction_wall_clock_time_limit: Duration::from_millis(50),
            ..DEFAULT_GRAPH_WRITER_MESSAGE_TRANSACTION_LIMITS
        };
    let slow = case
        .run_in_transaction_with_limits(
            graph_writer_transaction_limits_with_50_ms_total_time_budget,
            |scoped| async move {
                Case::walk_in(&scoped, walk).await?;
                tokio::time::sleep(Duration::from_millis(500)).await;
                Ok(())
            },
        )
        .await;

    let error = slow.expect_err("after 1: the transaction is past its budget");
    assert!(
        error.to_string().contains("graph transaction exceeded"),
        "after 1: {error:#}"
    );
    assert!(
        crate::writer::derivation_save_failure_user_message(&error)
            .contains("database transaction ran longer than the"),
        "after 1: user message for {error:#}"
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

#[tokio::test]
async fn m10_two_batches_of_one_evaluation_queued_together_are_written_in_one_pass() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;
    let gv = case.gv(e1).await;
    let of_a = case.tasked(e1, vec![entry_point(A, A_OUT, "pkg.a")]).await;
    let of_c = case.tasked(e1, vec![entry_point(C, C_OUT, "pkg.c")]).await;

    let (first, second) = together(case.walk_result(of_a), case.walk_result(of_c)).await;

    let first = first.expect("after 1: first result ok");
    let second = second.expect("after 1: second result ok");
    assert_eq!(first.walked, 2, "after 1: first report covers the group");
    assert_eq!(second.walked, 2, "after 1: second report covers the group");
    let (a, c) = (case.find_derivation(A).await, case.find_derivation(C).await);
    assert!(a.is_some(), "after 1: derivation(A) present");
    assert!(c.is_some(), "after 1: derivation(C) present");
    assert_eq!(case.gv(e1).await, gv + 1, "after 1: gv(E1) +1");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn m11_a_bad_batch_fails_the_whole_group_of_its_evaluation_and_every_reply() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = Case::start(shape().evaluation(e1).evaluation(e2)).await;
    let (gv1, gv2) = (case.gv(e1).await, case.gv(e2).await);
    let above_bad = with_attr(with_output(drv(C, &["bad"]), C_OUT), "pkg.c");
    let of_e1_good = case.tasked(e1, vec![entry_point(A, A_OUT, "pkg.a")]).await;
    let of_e1_bad = case.tasked(e1, vec![above_bad]).await;
    let of_e2 = case.tasked(e2, vec![entry_point(B, B_OUT, "pkg.b")]).await;

    let (good, bad, other) = tokio::join!(
        case.walk_result(of_e1_good),
        case.walk_result(of_e1_bad),
        case.walk_result(of_e2)
    );

    assert!(good.is_err(), "after 1: good batch of E1 error: {good:?}");
    assert!(bad.is_err(), "after 1: bad batch of E1 error: {bad:?}");
    assert!(other.is_ok(), "after 1: batch of E2 ok: {other:?}");
    let (a, b) = (case.find_derivation(A).await, case.find_derivation(B).await);
    assert_eq!(a, None, "after 1: no derivation(A)");
    assert!(b.is_some(), "after 1: derivation(B) present");
    assert_eq!(case.gv(e1).await, gv1, "after 1: gv(E1) unchanged");
    assert_eq!(case.gv(e2).await, gv2 + 1, "after 1: gv(E2) +1");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn m12_graph_writer_retries_row_lock_then_fails_when_lock_never_released() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(walked_a_above_a_stub(e1)).await;
    let gv = case.gv(e1).await;
    let walk_of_b = &case.tasked(e1, vec![with_output(drv(B, &[]), B_OUT)]).await;
    let graph_writer_transaction_limits_with_50ms_lock_wait_timeout =
        GraphWriterTransactionLimits {
            statement_lock_wait_timeout: Duration::from_millis(50),
            ..DEFAULT_GRAPH_WRITER_MESSAGE_TRANSACTION_LIMITS
        };

    let derivation_b_build_row_lock_transaction =
        open_second_transaction_and_lock_derivation_build_row(&case, B).await;
    let attempts = &AtomicU32::new(0);
    let never_released = case
        .run_in_transaction_with_limits(
            graph_writer_transaction_limits_with_50ms_lock_wait_timeout,
            |scoped| async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Case::walk_in(&scoped, walk_of_b).await
            },
        )
        .await;

    let error = never_released.expect_err("after 1: error");
    assert_eq!(
        postgres_sqlstate_from_error_chain(&error).as_deref(),
        Some("55P03"),
        "after 1: root cause is a lock timeout: {error:#}"
    );
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        GRAPH_TX_ATTEMPTS,
        "after 1: error once every attempt failed"
    );
    let b = case.derivation(B).await;
    assert!(!b.walked, "after 1: derivation(B) is still a stub");
    assert_eq!(case.gv(e1).await, gv, "after 1: gv(E1) unchanged");
    derivation_b_build_row_lock_transaction
        .rollback()
        .await
        .expect("second transaction releases that lock");
    case.zero_drift("step 1").await;

    let derivation_b_build_row_lock_transaction = &Mutex::new(Some(
        open_second_transaction_and_lock_derivation_build_row(&case, B).await,
    ));
    let attempts = &AtomicU32::new(0);
    let released = case
        .run_in_transaction_with_limits(
            graph_writer_transaction_limits_with_50ms_lock_wait_timeout,
            |scoped| async move {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                if attempt == 3 {
                    let holder = derivation_b_build_row_lock_transaction
                        .lock()
                        .expect("holder mutex poisoned")
                        .take();
                    if let Some(holder) = holder {
                        holder.rollback().await?;
                    }
                }
                Case::walk_in(&scoped, walk_of_b).await
            },
        )
        .await;

    released.expect("after 2: ok");
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        3,
        "after 2: commit on the third attempt"
    );
    let b = case.derivation(B).await;
    assert!(b.walked, "after 2: derivation(B) walked");
    assert_eq!(case.gv(e1).await, gv + 1, "after 2: gv(E1) +1 exactly");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn m13_old_holder_transaction_is_returned_and_recent_transaction_is_not() {
    let case = Case::start(shape()).await;
    let transaction_age_threshold = Duration::from_secs(1);

    let transaction_open_longer_than_age_threshold =
        open_transaction_and_set_application_name(&case, "holder").await;
    tokio::time::sleep(transaction_age_threshold * 3).await;
    let young_open_transaction = open_transaction_and_set_application_name(&case, "young").await;
    let postgres_session_rows_with_old_transactions =
        find_sessions_with_transaction_older_than(&case.ctx.worker_db, transaction_age_threshold)
            .await
            .expect("after 1: the session query runs");

    let holder = postgres_session_rows_with_old_transactions
        .iter()
        .find(|session| session.application_name.as_deref() == Some("holder"))
        .expect("after 1: the holder transaction is listed");
    assert!(
        holder.current_transaction_age_milliseconds >= 1000,
        "after 1: holder age {}",
        holder.current_transaction_age_milliseconds
    );
    assert_eq!(
        holder.state.as_deref(),
        Some("idle in transaction"),
        "after 1: holder state"
    );
    assert!(
        postgres_session_rows_with_old_transactions
            .iter()
            .all(|session| session.application_name.as_deref() != Some("young")),
        "after 1: the young transaction is not listed"
    );
    young_open_transaction
        .rollback()
        .await
        .expect("young transaction rolls back");
    transaction_open_longer_than_age_threshold
        .rollback()
        .await
        .expect("holder transaction rolls back");
    case.finish().await;
}

async fn open_transaction_and_set_application_name(
    case: &Case,
    application_name: &str,
) -> DatabaseTransaction {
    let transaction = case
        .ctx
        .worker_db
        .begin()
        .await
        .expect("open a transaction");
    transaction
        .execute_unprepared(&format!("SET application_name = '{application_name}'"))
        .await
        .expect("set the application name");
    transaction
}

#[tokio::test]
async fn m14_two_check_steps_repair_counters_and_a_large_scope_in_several_transactions() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(chain(e1)).await;
    let (a, b) = (case.db.id_of(A).await, case.db.id_of(B).await);
    case.sql(&format!(
        "UPDATE derivation SET unwalked_inputs = 3 WHERE id = '{a}'"
    ))
    .await;
    case.sql(&format!(
        "UPDATE derivation_build SET wanted = false WHERE derivation = '{b}'"
    ))
    .await;
    case.sql(NOTE_TRANSACTIONS).await;
    case.sql(DERIVATION_UPDATE_TRANSACTION_ID_TRIGGER_SQL).await;

    let report = case
        .graph
        .run_consistency_checks()
        .await
        .expect("after 1: run consistency check");

    assert_eq!(
        (report.walk_drift, report.need_drift),
        (1, 1),
        "after 1: {report:?}"
    );
    let unwalked =
        distinct_top_level_transaction_ids_that_changed_counter_column(&case, "unwalked_inputs")
            .await;
    let wanted =
        distinct_top_level_transaction_ids_that_changed_counter_column(&case, "wanted").await;
    assert_eq!(
        unwalked.len(),
        1,
        "after 1: repaired 1 transaction unwalked_inputs"
    );
    assert_eq!(wanted.len(), 1, "after 1: wanted = 1 repaired transaction");
    assert!(
        unwalked.is_disjoint(&wanted),
        "after 1: one commit per step, each in its own transaction"
    );
    case.zero_drift("step 1").await;

    let bulk = SWEEP_LOCK_CHUNK * SWEEP_CHUNKS_PER_TRANSACTION + 1;
    insert_many_created_shared_builds_with_wrong_blocking_deps(&case, bulk).await;

    let report = case
        .graph
        .run_consistency_checks()
        .await
        .expect("after 2: consistency check executed");

    assert!(
        report.counter_drift >= bulk as i64,
        "after 2: all drifted counters reported: {report:?}"
    );
    let blocking_deps =
        distinct_top_level_transaction_ids_that_changed_counter_column(&case, "blocking_deps")
            .await;
    assert!(
        blocking_deps.len() > 1,
        "after 2: repair the scope across multiple transactions: {blocking_deps:?}"
    );
    let drifted = case
        .db
        .texts(
            "SELECT count(*)::text FROM derivation_build db JOIN derivation d ON d.id = db.derivation \
             WHERE d.name = 'bulk' AND db.blocking_deps <> 0",
        )
        .await;
    assert_eq!(
        drifted,
        ["0"],
        "after 2: recount of blocking_deps for each inserted shared build"
    );
    case.zero_drift("step 2").await;
    case.finish().await;
}

const BUILD_JOB_WRITTEN_IN_TRIGGER_SQL: &str = "
    CREATE TRIGGER note_build_job AFTER INSERT ON build_job
        FOR EACH ROW EXECUTE FUNCTION note_transaction();
";

async fn delete_build_job_rows_for_derivation_store_paths(case: &Case, paths: &[&str]) {
    for path in paths {
        let derivation = case.db.id_of(path).await;
        case.sql(&format!(
            "DELETE FROM build_job WHERE derivation = '{derivation}'"
        ))
        .await;
    }
}

async fn derivation_ids_with_build_jobs_for_evaluation(
    case: &Case,
    evaluation: EvaluationId,
) -> BTreeSet<DerivationId> {
    case.jobs(evaluation)
        .await
        .into_iter()
        .map(|job| job.derivation)
        .collect()
}

#[tokio::test]
async fn consistency_check_adopts_orphaned_subtrees_under_live_evaluation_leaves_parentless_open_build_and_window_size_one_scan_matches_unbounded_scan()
 {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        shape()
            .evaluation(e1)
            .derivation(A)
            .walked()
            .entry_point("pkg.a")
            .job()
            .probed()
            .drv_cached()
            .output(A_OUT)
            .depends_on(B)
            .depends_on(D)
            .derivation(B)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .depends_on(C)
            .derivation(C)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(C_OUT)
            .build_status(BuildStatus::Queued)
            .derivation(D)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(D_OUT)
            .build_status(BuildStatus::Queued)
            .derivation(Z)
            .walked()
            .probed()
            .drv_cached()
            .output(Z_OUT)
            .build_status(BuildStatus::Skipped),
    )
    .await;
    delete_build_job_rows_for_derivation_store_paths(&case, &[B, C]).await;

    let report = case.drift().await;

    assert_eq!(report.adopted, 2, "After 1: {report:?}");
    assert!(
        case.job(e1, B).await.is_some() && case.job(e1, C).await.is_some(),
        "after 1: build jobs for B and C now belong to evaluation e1"
    );
    let z = case.db.id_of(Z).await;
    assert_eq!(
        case.db
            .texts(&format!(
                "SELECT count(*)::text FROM build_job WHERE derivation = '{z}'"
            ))
            .await,
        ["0"],
        "after 1: Z has no parent, so no names apply to it"
    );
    assert_eq!(
        case.sb(Z).await.status,
        BuildStatus::Skipped,
        "after 1: Z stays Skipped"
    );
    let report = case.drift().await;
    assert_eq!(
        report.total(),
        0,
        "after 1 run: second check found nothing to repair: {report:?}"
    );

    case.sql(NOTE_TRANSACTIONS).await;
    case.sql(BUILD_JOB_WRITTEN_IN_TRIGGER_SQL).await;
    delete_build_job_rows_for_derivation_store_paths(&case, &[B, C, D]).await;

    let one_open_build_per_transaction_consistency_check_report = case
        .graph
        .run_consistency_checks_scanning_open_builds_per_transaction(1)
        .await
        .expect("after 2: run a check per transaction to scan one open build");

    let transactions: BTreeSet<String> = transactions_writing(&case, "build_job")
        .await
        .into_iter()
        .collect();
    assert_eq!(
        transactions.len(),
        2,
        "after 2: two transactions name the subtrees under B and under D: {transactions:?}"
    );
    let derivation_ids_named_after_one_build_per_transaction_run =
        derivation_ids_with_build_jobs_for_evaluation(&case, e1).await;
    case.sql("DELETE FROM written_in").await;
    delete_build_job_rows_for_derivation_store_paths(&case, &[B, C, D]).await;

    let default_window_consistency_check_report = case
        .graph
        .run_consistency_checks()
        .await
        .expect("after 3: run the check with the default window");

    let transactions: BTreeSet<String> = transactions_writing(&case, "build_job")
        .await
        .into_iter()
        .collect();
    assert_eq!(
        transactions.len(),
        1,
        "after 3: one window can identify all builds in one transaction: {transactions:?}"
    );
    assert_eq!(
        (
            one_open_build_per_transaction_consistency_check_report.adopted,
            derivation_ids_named_after_one_build_per_transaction_run,
        ),
        (
            default_window_consistency_check_report.adopted,
            derivation_ids_with_build_jobs_for_evaluation(&case, e1).await,
        ),
        "after 3 rounds: both window sizes adopt the same pair"
    );
    assert_eq!(
        default_window_consistency_check_report.adopted, 3,
        "after 3 rounds: both window sizes adopt the same pair"
    );
    case.finish().await;
}

#[tokio::test]
async fn row_lock_timeout_in_first_step_still_repairs_wanted_counter_of_other_build() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(chain(e1)).await;
    let (a, b) = (case.db.id_of(A).await, case.db.id_of(B).await);
    case.sql(&format!(
        "UPDATE derivation SET unwalked_inputs = 3 WHERE id = '{a}'"
    ))
    .await;
    case.sql(&format!(
        "UPDATE derivation_build SET wanted = false WHERE derivation = '{b}'"
    ))
    .await;
    let holder = case
        .ctx
        .worker_db
        .begin()
        .await
        .expect("open a second transaction");
    let locked = holder
        .execute_unprepared(&format!(
            "SELECT 1 FROM derivation WHERE id = '{a}' FOR UPDATE"
        ))
        .await
        .map(|done| done.rows_affected());
    assert_eq!(
        locked.as_ref().ok(),
        Some(&1),
        "One lock on the derivation row for A"
    );

    let report = case
        .graph
        .run_consistency_checks()
        .await
        .expect("After 1: skip the locked step and end the check");

    assert_eq!(
        (report.walk_drift, report.need_drift),
        (0, 1),
        "after 1: {report:?}"
    );
    holder
        .rollback()
        .await
        .expect("second transaction releases that lock");

    let report = case
        .graph
        .run_consistency_checks()
        .await
        .expect("after 2: run consistency check");

    assert_eq!(
        (report.walk_drift, report.need_drift),
        (1, 0),
        "after 2: {report:?}"
    );
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn m15_two_batches_of_different_evaluations_committed_in_separate_transactions_let_the_first_reply_and_rows_be_visible_while_a_lock_holds_only_the_second()
 {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = Case::start(walked_a_above_a_stub(e2).evaluation(e1)).await;
    case.sql(NOTE_TRANSACTIONS).await;
    let of_e1 = case.tasked(e1, vec![entry_point(C, C_OUT, "pkg.c")]).await;
    let of_e2 = case.tasked(e2, vec![entry_point(D, D_OUT, "pkg.d")]).await;

    let (first, second) = together(case.walk_result(of_e1), case.walk_result(of_e2)).await;

    first.expect("after 1: first result ok");
    second.expect("after 1: second result ok");
    let transactions: BTreeSet<String> = transactions_writing(&case, "derivation")
        .await
        .into_iter()
        .collect();
    assert_eq!(
        transactions.len(),
        2,
        "after 1: commit the E1 batch and the E2 batch in two separate transactions: {transactions:?}"
    );
    case.zero_drift("step 1").await;

    let derivation_b_build_row_lock_transaction =
        open_second_transaction_and_lock_derivation_build_row(&case, B).await;
    let of_e1 = case.tasked(e1, vec![entry_point(Z, Z_OUT, "pkg.z")]).await;
    let of_e2 = case.tasked(e2, vec![with_output(drv(B, &[]), B_OUT)]).await;
    let mut second = Box::pin(case.walk_result(of_e2));

    let (first, second_sent) = together(
        tokio::time::timeout(
            DEFAULT_GRAPH_WRITER_MESSAGE_TRANSACTION_LIMITS.statement_lock_wait_timeout / 2,
            case.walk_result(of_e1),
        ),
        poll_once_and_check_still_pending(second.as_mut()),
    )
    .await;

    first
        .expect("after 2: E1 responded. E2 waited for the lock.")
        .expect("after 2: the first result is OK");
    assert!(
        second_sent,
        "after 2: E2 batch went out and no reply came back yet"
    );
    let z = case.find_derivation(Z).await;
    assert!(z.is_some(), "After 2: derivation(Z) is present");
    let waiting = poll_once_and_check_still_pending(second.as_mut()).await;
    assert!(waiting, "After 2: batches from E2 wait for the lock");
    let b = case.derivation(B).await;
    assert!(!b.walked, "after 2: derivation(B) is still a stub");
    derivation_b_build_row_lock_transaction
        .rollback()
        .await
        .expect("second transaction releases that lock");

    second.await.expect("After 3: second result is OK");
    let b = case.derivation(B).await;
    assert!(b.walked, "After 3: derivation(B) walked");
    case.zero_drift("step 3").await;
    case.finish().await;
}

#[tokio::test]
async fn call_while_graph_writer_is_down_reaches_restarted_graph_writer() {
    let mut case = Case::start(shape().evaluation(EvaluationId::now_v7())).await;
    case.writer
        .kill_and_wait(None)
        .await
        .expect("The test killed the graph writer.");
    let graph_writer_restart_task = async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        case.graph
            .spawn(case.ctx.clone(), None, None)
            .await
            .expect("Restart the graph writer.")
    };
    let (writer, ()) = tokio::join!(graph_writer_restart_task, async {
        case.graph
            .upstream_probed(Vec::new())
            .await
            .expect("Calls made during a restart reach the restarted graph writer.");
    });

    case.writer = writer;
    case.finish().await;
}

#[tokio::test]
async fn test_failed_signing_transaction_still_commits_demotion_of_completed_build_with_missing_stored_output_and_writes_no_signature()
 {
    let (case, c1) =
        start_case_with_completed_dependency_without_stored_output_and_completed_dependency_with_stored_output()
            .await;

    let parent_builds =
        crate::claims::read_page_of_parent_builds_with_dependency_outputs_to_publish(
            &case.ctx.worker_db,
            DerivationId::nil(),
            DerivationId::new(uuid::Uuid::max()),
            gradient_db::caches::demotion::MAX_PUBLISHED_PARENT_BUILDS_PER_SIGNING_PASS,
        )
        .await
        .expect("Read the parent build page");
    let signed_dependency_output_rows =
        crate::claims::select_and_sign_dependency_outputs_of_parent_build_page(
            &case.ctx,
            &parent_builds,
        )
        .await
        .expect("Select dependency outputs and sign them.");
    let failed_signing = inject_error(&case, "23505", Injection::Returned, 1..=1, |scoped| {
        let signed_dependency_output_rows = signed_dependency_output_rows.clone();
        async move {
            crate::transition::apply(
                &scoped,
                Transition::SignStoredInputsIntoReadWriteBinaryCache {
                    signed_dependency_output_rows_to_insert: signed_dependency_output_rows,
                },
            )
            .await
        }
    })
    .await;
    assert!(
        failed_signing.result.is_err(),
        "The signing pass returned the injected error after 1."
    );

    case.graph
        .run_consistency_checks()
        .await
        .expect("the consistency check is written");

    assert_ne!(
        case.sb(B).await.status,
        BuildStatus::Completed,
        "After 2: the consistency check demoted build B. The demotion persisted in the committed state."
    );
    assert!(
        !case
            .signatures(D_OUT)
            .await
            .iter()
            .any(|row| row.cache == c1 && row.signature.is_some()),
        "after 2: the failed signing pass left no signature of D_OUT in the cache"
    );
    case.finish_with_drift().await;
}

async fn start_case_with_completed_dependency_without_stored_output_and_completed_dependency_with_stored_output()
-> (Case, CacheId) {
    let (stored_build_outputs_owner_evaluation_id, e1) =
        (EvaluationId::now_v7(), EvaluationId::now_v7());
    let c1 = CacheId::now_v7();
    let seed = shape()
        .evaluation(stored_build_outputs_owner_evaluation_id)
        .cached_path(D_OUT, "d")
        .evaluation(e1)
        .evaluation_status(EvaluationStatus::Building)
        .into_cache(c1)
        .keyed(Case::key());
    let seed = add_waiting_build_that_depends_on_completed_build(
        seed,
        (A, A_OUT),
        (B, B_OUT),
        BuildStatus::Created,
    );
    let seed = add_waiting_build_that_depends_on_completed_build(
        seed,
        (C, C_OUT),
        (D, D_OUT),
        BuildStatus::Queued,
    );
    (Case::start_with_unstored_outputs(seed, 1).await, c1)
}

#[tokio::test]
async fn test_first_recount_postgres_lock_timeout_still_commits_demotion_of_completed_build_without_stored_output_copy()
 {
    let (case, _) =
        start_case_with_completed_dependency_without_stored_output_and_completed_dependency_with_stored_output()
            .await;
    let c = case.db.id_of(C).await;
    case.sql(&format!(
        "UPDATE derivation SET unwalked_inputs = 3 WHERE id = '{c}'"
    ))
    .await;
    let holder = case
        .ctx
        .worker_db
        .begin()
        .await
        .expect("A second transaction can open.");
    let locked = holder
        .execute_unprepared(&format!(
            "SELECT 1 FROM derivation WHERE id = '{c}' FOR UPDATE"
        ))
        .await
        .map(|done| done.rows_affected());
    assert_eq!(
        locked.as_ref().ok(),
        Some(&1),
        "One lock on the derivation row of C"
    );

    let report = case
        .graph
        .run_consistency_checks()
        .await
        .expect("Consistency checks skip locked steps and exit after 1.");

    assert_eq!(
        report.walk_drift, 0,
        "after 1: the locked recount of C found no walk drift"
    );
    assert_ne!(
        case.sb(B).await.status,
        BuildStatus::Completed,
        "After 1: B's demotion went through even though the first recheck hit a lock timeout"
    );
    holder
        .rollback()
        .await
        .expect("The second transaction can release the lock.");
    case.finish_with_drift().await;
}

#[tokio::test]
async fn test_completed_build_demotion_postgres_lock_timeout_still_repairs_walk_counter_and_demotes_after_lock_release()
 {
    let (case, _) =
        start_case_with_completed_dependency_without_stored_output_and_completed_dependency_with_stored_output()
            .await;
    let (b, c) = (case.db.id_of(B).await, case.db.id_of(C).await);
    case.sql(&format!(
        "UPDATE derivation SET unwalked_inputs = 3 WHERE id = '{c}'"
    ))
    .await;
    let holder = case
        .ctx
        .worker_db
        .begin()
        .await
        .expect("A second transaction can open.");
    let locked = holder
        .execute_unprepared(&format!(
            "SELECT 1 FROM derivation_build WHERE derivation = '{b}' FOR UPDATE"
        ))
        .await
        .map(|done| done.rows_affected());
    assert_eq!(
        locked.as_ref().ok(),
        Some(&1),
        "One lock on the derivation_build row for B."
    );

    let report = case
        .graph
        .run_consistency_checks()
        .await
        .expect("after 1: demotion reached the lock timeout. The consistency check steps still ran to the end.");

    assert_eq!(
        report.walk_drift, 1,
        "The consistency check step repaired C's walk counter after 1."
    );
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Completed,
        "B is still Completed after step 1. A lock is on the `derivation_build` row of B."
    );
    holder
        .rollback()
        .await
        .expect("The second transaction can release the lock.");

    case.graph
        .run_consistency_checks()
        .await
        .expect("After 2: consistency check");

    assert_ne!(
        case.sb(B).await.status,
        BuildStatus::Completed,
        "After 2: the next round demoted B."
    );
    case.finish_with_drift().await;
}

#[tokio::test]
async fn test_completed_build_demotion_non_lock_database_error_returns_round_report_with_walk_drift_one()
 {
    let (case, _) =
        start_case_with_completed_dependency_without_stored_output_and_completed_dependency_with_stored_output()
            .await;
    let (b, c) = (case.db.id_of(B).await, case.db.id_of(C).await);
    case.sql(&format!(
        "UPDATE derivation SET unwalked_inputs = 3 WHERE id = '{c}'; \
         CREATE FUNCTION reject_derivation_build_status_change_for_build_b() \
         RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
           RAISE EXCEPTION 'injected'; \
         END $$; \
         CREATE TRIGGER reject_build_b_status_change_before_derivation_build_update \
           BEFORE UPDATE ON derivation_build FOR EACH ROW \
           WHEN (OLD.derivation = '{b}' AND OLD.status IS DISTINCT FROM NEW.status) \
           EXECUTE FUNCTION reject_derivation_build_status_change_for_build_b();"
    ))
    .await;

    let error =
        case.graph.run_consistency_checks().await.expect_err(
            "After 1: the consistency check returned the error from the failed demotion.",
        );

    assert_eq!(
        error
            .downcast_ref::<crate::ConsistencyCheckDemotionFailed>()
            .and_then(|failed| failed.consistency_report_after_failed_demotion)
            .map(|report| report.walk_drift),
        Some(1),
        "After 1: demotion failed with an error other than a lock timeout. The consistency check still repaired C's walk counter and returned the demotion error with the report."
    );
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Completed,
        "After 1: the demotion failed. B remains Completed."
    );
    case.sql(
        "DROP TRIGGER reject_build_b_status_change_before_derivation_build_update ON derivation_build",
    )
    .await;
    case.finish_with_drift().await;
}

async fn test_case_with_finished_walk(evaluation: EvaluationId) -> Case {
    let case = Case::start(shape().evaluation(evaluation).only_evaluate()).await;
    case.walk(
        case.tasked(evaluation, vec![with_attr(drv(A, &[]), "pkg.a")])
            .await,
    )
    .await;
    let _ = tokio::time::timeout(
        Duration::from_millis(10),
        case.ctx.wake.run_signature_sweep_now.notified(),
    )
    .await;
    case
}

fn build_evaluation_stream_completed_transition(evaluation: EvaluationId) -> Transition {
    Transition::EvalStreamCompleted { evaluation }
}

#[tokio::test]
async fn finishing_evaluation_wakes_signature_sweep_after_commit_within_one_second() {
    let e1 = EvaluationId::now_v7();
    let case = test_case_with_finished_walk(e1).await;
    let sweep = Arc::clone(&case.ctx.wake.run_signature_sweep_now);
    let connection = case.db.connection();
    let woken = Shutdown::new().spawn(async move {
        sweep.notified().await;
        let woken_at = Instant::now();
        let evaluation = gradient_entity::evaluation::Entity::find_by_id(e1)
            .one(&connection)
            .await
            .expect("The evaluation row is readable.")
            .expect("The evaluation is present.");
        (woken_at, evaluation.status)
    });

    case.transition(build_evaluation_stream_completed_transition(e1))
        .await;
    let returned_at = Instant::now();

    let (woken_at, status_when_woken) = tokio::time::timeout(Duration::from_secs(1), woken)
        .await
        .expect("The signature sweep got a wake-up within 1 second after the transition returned.")
        .expect("Task waiting for wake exited.");
    match woken_at.checked_duration_since(returned_at) {
        Some(delay) => println!(
            "The wake for the signature sweep arrived {delay:?} after the transition returned."
        ),
        None => println!(
            "The wake for the signature sweep arrived {:?} before the transition returned.",
            returned_at.duration_since(woken_at)
        ),
    }
    assert_eq!(
        status_when_woken,
        EvaluationStatus::Completed,
        "The second connection read the evaluation as Completed after the wake arrived."
    );
    case.finish().await;
}

#[tokio::test]
async fn failed_commit_of_finished_evaluation_sends_no_signature_sweep_wake() {
    let e1 = EvaluationId::now_v7();
    let case = test_case_with_finished_walk(e1).await;
    case.sql(
        "CREATE FUNCTION fail_commit_when_evaluation_is_finished() \
         RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
           RAISE EXCEPTION 'injected'; \
         END $$; \
         CREATE CONSTRAINT TRIGGER fail_commit_when_evaluation_is_finished \
           AFTER UPDATE ON evaluation DEFERRABLE INITIALLY DEFERRED FOR EACH ROW \
           WHEN (OLD.finished_at IS NULL AND NEW.finished_at IS NOT NULL) \
           EXECUTE FUNCTION fail_commit_when_evaluation_is_finished();",
    )
    .await;

    case.graph
        .transition(build_evaluation_stream_completed_transition(e1))
        .await
        .expect_err("The transaction to end the evaluation failed to commit.");

    assert!(
        tokio::time::timeout(
            Duration::from_millis(200),
            case.ctx.wake.run_signature_sweep_now.notified(),
        )
        .await
        .is_err(),
        "A transaction that failed to commit did not start the signature sweep."
    );
    assert_ne!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Completed,
        "Evaluation isn't Completed after a failed commit."
    );
    case.sql("DROP TRIGGER fail_commit_when_evaluation_is_finished ON evaluation")
        .await;
    case.finish().await;
}
