/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus::{Building, FailedTransient};
use gradient_entity::build_attempt::AttemptOutcome;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::EvaluationId;
use gradient_fixtures::shape::shape;
use gradient_wire::types::BuildFailureKind;

use super::{A, A_OUT, B, B_OUT, C, C_OUT, Case, D, D_OUT, X, X_OUT, drv, store_path, with_output};
use crate::test_ctx::BatchReport;

async fn walk_leaf(case: &Case, evaluation: EvaluationId, path: &str, output: &str) -> BatchReport {
    let leaf = with_output(drv(path, &[]), output);
    case.walk(case.tasked(evaluation, vec![leaf]).await).await
}

async fn unwalked_inputs(case: &Case, paths: &[&str]) -> Vec<i32> {
    let mut inputs = Vec::new();
    for path in paths {
        inputs.push(case.derivation(path).await.unwalked_inputs);
    }

    inputs
}

#[tokio::test]
async fn w01_unwalked_inputs_reads_build_inputs_only_and_ignores_a_runtime_stub() {
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
            .runtime_depends_on(B)
            .depends_on(C)
            .derivation(B)
            .job()
            .derivation(C)
            .job(),
    )
    .await;

    let known = case.known(&[A, B, C]).await;

    assert!(known.is_empty(), "after 1: known_derivations: {known:?}");
    case.zero_drift("step 1").await;

    walk_leaf(&case, e1, C, C_OUT).await;

    let inputs = unwalked_inputs(&case, &[A]).await;
    assert_eq!(inputs, [0], "after 2: unwalked_inputs of A");
    case.zero_drift("step 2").await;

    let known = case.known(&[A, B, C]).await;

    assert_eq!(known, [A, C], "after 3: known_derivations");
    case.zero_drift("step 3").await;
    case.finish().await;
}

#[tokio::test]
async fn w02_a_chain_completes_level_by_level_in_a_single_batch() {
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
            .derivation(B)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .depends_on(C)
            .derivation(C)
            .job(),
    )
    .await;

    walk_leaf(&case, e1, C, C_OUT).await;

    let inputs = unwalked_inputs(&case, &[C, B, A]).await;
    assert_eq!(inputs, [0, 0, 0], "after 1: unwalked_inputs of C, B, A");
    let known = case.known(&[A, B, C]).await;
    assert_eq!(known, [A, B, C], "after 1: known_derivations");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn w03_both_unknown_inputs_of_a_diamond_top_drop_with_its_leaf() {
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
            .depends_on(C)
            .derivation(B)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .depends_on(D)
            .derivation(C)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(C_OUT)
            .depends_on(D)
            .derivation(D)
            .job(),
    )
    .await;

    let diamond = [A, B, C, D];
    let inputs = unwalked_inputs(&case, &diamond).await;
    assert_eq!(inputs, [2, 1, 1, 0], "given: unwalked_inputs of A to D");

    walk_leaf(&case, e1, D, D_OUT).await;

    let inputs = unwalked_inputs(&case, &diamond).await;
    assert_eq!(inputs, [0; 4], "after 1: unwalked_inputs of A to D");
    case.zero_drift("step 1").await;

    walk_leaf(&case, e1, D, D_OUT).await;

    let inputs = unwalked_inputs(&case, &diamond).await;
    assert_eq!(inputs, [0; 4], "after 2: unwalked_inputs of A to D");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn w04_a_missing_producerless_drv_leaves_its_derivation_walked_and_the_batch_is_skipped() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        shape()
            .evaluation(e1)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(X)
            .walked()
            .entry_point("pkg.x")
            .job()
            .probed()
            .drv_cached()
            .output(X_OUT)
            .depends_on(A)
            .derivation(A)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(A_OUT)
            .build_status(Building)
            .attempt(AttemptOutcome::Running, None),
    )
    .await;

    let (drv_hash, package) = A.split_once('-').expect("A is a drv path");
    let missing = [store_path(drv_hash, package)];
    let kind = BuildFailureKind::InputsUnavailable;

    case.build_failed(A, kind, &missing).await;

    let stored = case.find_cached_path(drv_hash).await;
    assert_eq!(stored, None, "after 1: cached_path(A.drv)");
    let derivation = case.derivation(A).await;
    assert!(derivation.walked, "after 1: derivation(A).walked");
    let inputs = unwalked_inputs(&case, &[X]).await;
    assert_eq!(inputs, [0], "after 1: unwalked_inputs of X");
    let a = case.sb(A).await;
    let columns = (a.status, a.attempt, a.wanted);
    assert_eq!(columns, (FailedTransient, 1, true), "after 1: sb(A)");
    let outcome = case.latest_attempt(A).await.outcome;
    assert_eq!(outcome, AttemptOutcome::Failed, "after 1: latest attempt");
    let blocking_deps = case.sb(X).await.blocking_deps;
    assert_eq!(blocking_deps, 1, "after 1: sb(X).blocking_deps");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 2, 0, 0, 0], "after 1: counters(E1)");
    case.zero_drift("step 1").await;
    let rows = case.rows().await;

    let report = walk_leaf(&case, e1, A, A_OUT).await;

    let columns = (report.skipped, report.walked);
    assert_eq!(columns, (true, 0), "after 2: report");
    assert_eq!(case.rows().await, rows, "after 2: no row changes");
    case.zero_drift("step 2").await;
    case.finish().await;
}
