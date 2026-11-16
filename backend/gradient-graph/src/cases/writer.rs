/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeSet;
use std::ops::RangeInclusive;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use gradient_db::connection::find_sessions_with_transaction_older_than;
use gradient_db::graph::can_start::{SWEEP_CHUNKS_PER_TRANSACTION, SWEEP_LOCK_CHUNK};
use gradient_db::{DbContext, MissingRow};
use gradient_entity::StorePathError;
use gradient_entity::build::BuildStatus;
use gradient_entity::entry_point::{License, Licenses};
use gradient_entity::ids::{DerivationBuildId, DerivationId, DispatchedJobId, EvaluationId};
use gradient_entity::{derivation, derivation_build};
use gradient_fixtures::shape::shape;
use gradient_fixtures::test_date;
use gradient_wire::types::{self, DiscoveredDerivation};
use sea_orm::{
    ConnectionTrait, DatabaseTransaction, EntityTrait, IntoActiveModel, TransactionTrait,
};

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
