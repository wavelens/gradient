/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::time::Duration;

use gradient_entity::build::BuildStatus::{Building, Completed, Created, Queued, Skipped};
use gradient_entity::build_attempt::AttemptOutcome;
use gradient_entity::cached_path;
use gradient_entity::derivation_dependency::EdgeKind;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::EvaluationId;
use gradient_fixtures::shape::{Shape, shape};
use sea_orm::sea_query::LockType;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect, TransactionTrait};

use super::{
    A, A_OUT, B, B_OUT, B_OUT2, C, C_OUT, Case, D, D_OUT, F_OUT, F1, F2, M, M_OUT, P, P_OUT,
    SOURCE, X, X_OUT, assert_only_unstored_outputs, chain, chain_done, commit_of, drv_nar, nar,
};
use crate::messages::NarCommit;

fn chain_with_b_building(evaluation: EvaluationId) -> Shape {
    chain(evaluation)
        .derivation(B)
        .build_status(Building)
        .attempt(AttemptOutcome::Running, None)
}

fn runtime_chain_below_x(evaluation: EvaluationId) -> Shape {
    shape()
        .evaluation(evaluation)
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
        .build_status(Completed)
        .runtime_depends_on(B)
        .derivation(B)
        .walked()
        .job()
        .probed()
        .drv_cached()
        .output(B_OUT)
        .build_status(Completed)
        .runtime_depends_on(C)
        .derivation(C)
        .walked()
        .job()
        .probed()
        .drv_cached()
        .output(C_OUT)
        .build_status(Completed)
}

async fn end_state_of_the_runtime_chain(case: &Case, evaluation: EvaluationId) -> Vec<String> {
    let mut state = Vec::new();
    for path in [X, A, B, C] {
        let build = case.sb(path).await;
        let queue = (build.status, build.cache_available, build.probed);
        let closure = (build.fetchable, build.missing_runtime_deps);
        let cached = case.output_of(path, "out").await.is_cached;
        let pending = (build.blocking_deps, build.attempt);
        let columns = format!("{queue:?} {closure:?} {pending:?} {cached}");
        state.push(format!("{path}: {columns}"));
    }

    for (from, to) in [(X, A), (A, B), (B, C)] {
        let kind = case.edge_kind(from, to).await;
        state.push(format!("{from} to {to}: {kind:?}"));
    }

    for hash in [A_OUT, B_OUT, C_OUT] {
        let path = case.cached_path(hash).await;
        let stored = (path.confirmed, path.references);
        state.push(format!("{hash}: {stored:?}"));
    }

    state.push(format!("sb(X).wanted: {}", case.sb(X).await.wanted));
    state.push(format!("counters: {:?}", case.counters(evaluation).await));
    state
}

#[tokio::test]
async fn n01_committing_the_output_of_a_completed_leaf_makes_it_fetchable_and_queues_the_parent() {
    let e1 = EvaluationId::now_v7();
    let seed = chain(e1).derivation(B).build_status(Completed);
    let case = Case::start_with_unstored_outputs(seed, 1).await;
    let gv = case.gv(e1).await;

    let report = case
        .commit_nar(NarCommit {
            file_hash: "sha256:fh".into(),
            built_by_worker: true,
            ..commit_of(B_OUT, "b", &[(B_OUT, "b")])
        })
        .await;

    let path = case.cached_path(B_OUT).await;
    assert!(path.confirmed, "after 1: cached_path(B_OUT).confirmed");
    let file_hash = path.file_hash.as_deref();
    assert_eq!(file_hash, Some("sha256:fh"), "after 1: file_hash");
    let references = Some(format!("{B_OUT}-b"));
    assert_eq!(path.references, references, "after 1: references");
    let signatures = case.signatures(B_OUT).await;
    assert!(signatures.is_empty(), "after 1: {signatures:?}");
    let output = case.output_of(B, "out").await;
    assert!(output.is_cached, "after 1: output(B,out).is_cached");
    let linked = output.cached_path;
    assert_eq!(linked, Some(path.id), "after 1: output(B,out) of B_OUT");
    let report = (report.created, report.outputs_marked);
    assert_eq!(report, (true, 1), "after 1: report");
    let b = case.sb(B).await;
    let b = (b.status, b.missing_runtime_deps, b.fetchable);
    assert_eq!(b, (Completed, 0, true), "after 1: sb(B)");
    let a = case.queue_state(A).await;
    assert_eq!(a, (Queued, 0), "after 1: sb(A)");
    assert_eq!(case.gv(e1).await, gv + 1, "after 1: gv(E1) +1");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 1, 0, 1, 0], "after 1: counters(E1)");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn n02_a_commit_before_completion_ends_in_the_state_of_a_commit_after_it() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(chain_with_b_building(e1)).await;

    case.commit_nar(commit_of(B_OUT, "b", &[(B_OUT, "b")]))
        .await;

    let b = case.sb(B).await;
    let b = (b.status, b.missing_runtime_deps, b.fetchable, b.wanted);
    assert_eq!(b, (Building, 0, false, true), "after 1: sb(B)");
    let cached = case.output_of(B, "out").await.is_cached;
    assert!(cached, "after 1: output(B,out).is_cached");
    let a = case.queue_state(A).await;
    assert_eq!(a, (Created, 1), "after 1: sb(A)");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 2, 0, 0, 1], "after 1: counters(E1)");
    case.zero_drift("step 1").await;

    case.build_completed(B).await;

    let b = case.sb(B).await;
    let b = (b.status, b.missing_runtime_deps, b.fetchable);
    assert_eq!(b, (Completed, 0, true), "after 2: sb(B)");
    let outcome = case.latest_attempt(B).await.outcome;
    let built = AttemptOutcome::Built;
    assert_eq!(outcome, built, "after 2: attempt(B, latest).outcome");
    let cached = case.output_of(B, "out").await.is_cached;
    assert!(cached, "after 2: output(B,out).is_cached as in N01");
    let a = case.queue_state(A).await;
    assert_eq!(a, (Queued, 0), "after 2: sb(A)");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 1, 0, 1, 0], "after 2: counters(E1)");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn n03_with_two_outputs_the_producer_is_present_only_after_the_second_commit() {
    let e1 = EvaluationId::now_v7();
    let seed = chain(e1)
        .derivation(B)
        .named_output(B_OUT2, "dev")
        .build_status(Completed);

    let case = Case::start_with_unstored_outputs(seed, 2).await;

    case.commit_nar(commit_of(B_OUT, "b", &[(B_OUT, "b")]))
        .await;

    let cached = case.output_of(B, "out").await.is_cached;
    assert!(cached, "after 1: output(B,out).is_cached");
    assert!(!case.sb(B).await.fetchable, "after 1: sb(B).fetchable");
    let a = case.queue_state(A).await;
    assert_eq!(a, (Created, 1), "after 1: sb(A)");
    assert_only_unstored_outputs(case.drift().await, 1, "step 1");

    let dev = commit_of(B_OUT2, "b-dev", &[(B_OUT2, "b-dev"), (B_OUT, "b")]);
    case.commit_nar(dev).await;

    let b = case.closure_state(B).await;
    assert_eq!(b, (true, 0), "after 2: sb(B) fetchable, missing 0");
    let a = case.queue_state(A).await;
    assert_eq!(a, (Queued, 0), "after 2: sb(A)");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn n04_a_completing_leaf_completes_the_closures_up_a_runtime_chain_in_a_single_commit() {
    let e1 = EvaluationId::now_v7();
    let seed = runtime_chain_below_x(e1).cached_output(A).cached_output(B);
    let case = Case::start_with_unstored_outputs(seed, 1).await;

    case.commit_nar(commit_of(C_OUT, "c", &[(C_OUT, "c")]))
        .await;

    for path in [C, B, A] {
        let closure = case.closure_state(path).await;
        assert_eq!(closure, (true, 0), "after 1: {path} fetchable, missing 0");
    }

    let x = case.queue_state(X).await;
    assert_eq!(x, (Queued, 0), "after 1: sb(X)");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [4, 1, 0, 1, 0], "after 1: counters(E1)");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn n05_the_commits_of_a_runtime_chain_end_in_the_same_state_in_every_order() {
    let commits = [
        commit_of(A_OUT, "a", &[(A_OUT, "a"), (B_OUT, "b")]),
        commit_of(B_OUT, "b", &[(B_OUT, "b"), (C_OUT, "c")]),
        commit_of(C_OUT, "c", &[(C_OUT, "c")]),
    ];

    let orders = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];

    let mut end_states = Vec::new();
    for order in orders {
        let e1 = EvaluationId::now_v7();
        let case = Case::start_with_unstored_outputs(runtime_chain_below_x(e1), 3).await;

        for (commit, unstored) in order.into_iter().zip([2, 1, 0]) {
            case.commit_nar(commits[commit].clone()).await;

            let line = format!("order {order:?}, {unstored} commits before the end");
            if unstored > 0 {
                let x = case.sb(X).await.status;
                assert_eq!(x, Created, "{line}: sb(X).status");
            }

            assert_only_unstored_outputs(case.drift().await, unstored, &line);
        }

        for path in [A, B, C] {
            let closure = case.closure_state(path).await;
            assert_eq!(closure, (true, 0), "order {order:?}, at the end: {path}");
        }

        let x = case.sb(X).await.status;
        assert_eq!(x, Queued, "order {order:?}, at the end: sb(X)");
        let counters = case.counters(e1).await;
        assert_eq!(counters, [4, 1, 0, 1, 0], "order {order:?}: counters");
        end_states.push(end_state_of_the_runtime_chain(&case, e1).await);
        case.finish().await;
    }

    for state in &end_states {
        assert_eq!(state, &end_states[0], "all six end states are equal");
    }
}

#[tokio::test]
async fn n06_a_reference_to_a_missing_path_leaves_the_producer_incomplete() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        chain_with_b_building(e1)
            .derivation(D)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(D_OUT)
            .build_status(Skipped),
    )
    .await;

    case.commit_nar(commit_of(B_OUT, "b", &[(B_OUT, "b"), (D_OUT, "d")]))
        .await;

    let edge = case.edge_kind(B, D).await;
    assert_eq!(edge, Some(EdgeKind::Runtime), "after 1: edge(B,D).kind");
    let b = case.sb(B).await;
    let b = (b.status, b.missing_runtime_deps, b.fetchable);
    assert_eq!(b, (Building, 1, false), "after 1: sb(B)");
    let d = case.sb(D).await;
    let d = (d.status, d.attempt, d.wanted);
    assert_eq!(d, (Created, 0, true), "after 1: sb(D)");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [3, 3, 0, 0, 1], "after 1: counters(E1)");
    case.zero_drift("step 1").await;

    case.build_completed(B).await;

    let b = case.sb(B).await;
    let b = (b.status, b.fetchable, b.wanted);
    assert_eq!(b, (Completed, false, true), "after 2: sb(B)");
    let a = case.queue_state(A).await;
    assert_eq!(a, (Created, 1), "after 2: sb(A)");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [3, 2, 0, 0, 0], "after 2: counters(E1)");
    case.zero_drift("step 2").await;

    case.commit_nar(commit_of(D_OUT, "d", &[(D_OUT, "d")]))
        .await;

    let d = case.sb(D).await;
    let d = (d.status, d.fetchable, d.missing_runtime_deps, d.wanted);
    assert_eq!(d, (Skipped, false, 0, false), "after 3: sb(D)");
    let b = case.sb(B).await;
    let b = (b.status, b.missing_runtime_deps, b.fetchable);
    assert_eq!(b, (Completed, 0, true), "after 3: sb(B)");
    let a = case.queue_state(A).await;
    assert_eq!(a, (Queued, 0), "after 3: sb(A)");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [3, 1, 0, 1, 0], "after 3: counters(E1)");
    case.zero_drift("step 3").await;
    case.finish().await;
}

#[tokio::test]
async fn n07_an_unconfirmed_row_is_present_and_its_confirming_recommit_can_drop_fetchable() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        chain_done(e1)
            .unconfirmed()
            .references(&[(B_OUT, "b")])
            .derivation(D)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(D_OUT)
            .build_status(Skipped),
    )
    .await;

    assert!(case.sb(B).await.fetchable, "given: sb(B) is present");

    case.commit_nar(NarCommit {
        file_hash: "sha256:fh2".into(),
        ..commit_of(B_OUT, "b", &[(B_OUT, "b"), (D_OUT, "d")])
    })
    .await;

    let path = case.cached_path(B_OUT).await;
    assert!(path.confirmed, "after 1: cached_path(B_OUT).confirmed");
    let file_hash = path.file_hash.as_deref();
    assert_eq!(file_hash, Some("sha256:fh2"), "after 1: file_hash");
    let references = Some(format!("{B_OUT}-b {D_OUT}-d"));
    assert_eq!(path.references, references, "after 1: references");
    let edge = case.edge_kind(B, D).await;
    assert_eq!(edge, Some(EdgeKind::Runtime), "after 1: edge(B,D).kind");
    let b = case.sb(B).await;
    let b = (b.status, b.missing_runtime_deps, b.fetchable, b.wanted);
    assert_eq!(b, (Completed, 1, false, true), "after 1: sb(B)");
    let a = case.queue_state(A).await;
    assert_eq!(a, (Created, 1), "after 1: sb(A)");
    let d = case.sb(D).await;
    let d = (d.status, d.attempt, d.wanted);
    assert_eq!(d, (Queued, 0, true), "after 1: sb(D), answers Q8");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [3, 2, 0, 1, 0], "after 1: counters, answers Q8");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn n08_references_are_stored_in_the_given_order_and_a_self_reference_adds_no_edge() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start_with_unstored_outputs(
        shape()
            .evaluation(e1)
            .derivation(B)
            .walked()
            .entry_point("pkg.b")
            .job()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .build_status(Completed)
            .runtime_depends_on(C)
            .derivation(C)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(C_OUT)
            .build_status(Completed)
            .cached_output(C),
        1,
    )
    .await;

    case.commit_nar(commit_of(B_OUT, "b", &[(C_OUT, "c"), (B_OUT, "b")]))
        .await;

    let references = case.cached_path(B_OUT).await.references;
    let given = Some(format!("{C_OUT}-c {B_OUT}-b"));
    assert_eq!(references, given, "after 1: references of B_OUT");
    let own = case.edge_kind(B, B).await;
    assert_eq!(own, None, "after 1: no edge(B,B)");
    let edge = case.edge_kind(B, C).await;
    assert_eq!(edge, Some(EdgeKind::Runtime), "after 1: edge(B,C).kind");
    let b = case.closure_state(B).await;
    assert_eq!(b, (true, 0), "after 1: sb(B) fetchable, missing 0");
    let status = case.evaluation(e1).await.status;
    let evaluating = EvaluationStatus::EvaluatingDerivation;
    assert_eq!(status, evaluating, "after 1: evaluation(E1).status");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn n09_a_single_path_backs_every_output_with_its_hash_and_both_producers_are_present() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start_with_unstored_outputs(
        shape()
            .evaluation(e1)
            .derivation(F1)
            .walked()
            .entry_point("pkg.f1")
            .job()
            .probed()
            .drv_cached()
            .output(F_OUT)
            .build_status(Completed)
            .derivation(F2)
            .walked()
            .entry_point("pkg.f2")
            .job()
            .probed()
            .drv_cached()
            .output(F_OUT)
            .build_status(Queued),
        1,
    )
    .await;

    let report = case
        .commit_nar(commit_of(F_OUT, "f1", &[(F_OUT, "f1")]))
        .await;

    assert_eq!(report.outputs_marked, 2, "after 1: report.outputs_marked");
    for path in [F1, F2] {
        let cached = case.output_of(path, "out").await.is_cached;
        assert!(cached, "after 1: output({path},out).is_cached");
    }

    assert!(case.sb(F1).await.fetchable, "after 1: sb(F1).fetchable");
    let f2 = case.sb(F2).await;
    let f2 = (f2.status, f2.fetchable, f2.missing_runtime_deps, f2.wanted);
    assert_eq!(f2, (Created, false, 0, true), "after 1: sb(F2)");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 1, 0, 0, 0], "after 1: counters(E1)");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn n10_a_drv_commit_queues_its_owner_only_when_the_other_conditions_are_met() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start_with_unstored_outputs(
        shape()
            .evaluation(e1)
            .derivation(A)
            .walked()
            .entry_point("pkg.a")
            .job()
            .probed()
            .output(A_OUT)
            .depends_on(B)
            .derivation(B)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .build_status(Completed),
        1,
    )
    .await;

    let (a_drv, _) = A.split_once('-').expect("A is a drv path");

    let report = case.commit_nar(drv_nar(A)).await;

    let stored = case.find_cached_path(a_drv).await;
    assert!(stored.is_some(), "after 1: cached_path(A.drv) exists");
    let a = case.queue_state(A).await;
    assert_eq!(a, (Created, 1), "after 1: sb(A)");
    assert_eq!(report.outputs_marked, 0, "after 1: report.outputs_marked");
    assert_only_unstored_outputs(case.drift().await, 1, "step 1");

    case.commit_nar(commit_of(B_OUT, "b", &[(B_OUT, "b")]))
        .await;

    let a = case.queue_state(A).await;
    assert_eq!(a, (Queued, 0), "after 2: sb(A)");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn n11_a_reference_onto_a_buildtime_edge_makes_it_both_and_a_runtime_dependency() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start_with_unstored_outputs(
        shape()
            .evaluation(e1)
            .derivation(X)
            .walked()
            .entry_point("pkg.x")
            .job()
            .probed()
            .drv_cached()
            .output(X_OUT)
            .depends_on(P)
            .derivation(P)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(P_OUT)
            .build_status(Completed)
            .depends_on(D)
            .depends_on(M)
            .derivation(D)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(D_OUT)
            .build_status(Completed)
            .cached_output(D)
            .derivation(M)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(M_OUT)
            .build_status(Completed),
        2,
    )
    .await;

    let references = [(P_OUT, "p"), (D_OUT, "d"), (M_OUT, "m")];
    case.commit_nar(commit_of(P_OUT, "p", &references)).await;

    for dependency in [D, M] {
        let edge = case.edge_kind(P, dependency).await;
        let line = format!("after 1: edge(P,{dependency}).kind");
        assert_eq!(edge, Some(EdgeKind::Both), "{line}");
    }

    let p = case.queue_state(P).await;
    assert_eq!(p, (Completed, 1), "after 1: sb(P) status, blocking_deps");
    let p = case.closure_state(P).await;
    assert_eq!(p, (false, 1), "after 1: sb(P) fetchable, missing 1");
    let x = case.queue_state(X).await;
    assert_eq!(x, (Created, 1), "after 1: sb(X)");
    assert!(case.sb(M).await.wanted, "after 1: sb(M).wanted");
    assert_only_unstored_outputs(case.drift().await, 1, "step 1");
    let end = case.finish_with_drift().await;
    assert_only_unstored_outputs(end, 1, "the end");
}

#[tokio::test]
async fn n12_a_path_nobody_produces_is_stored_without_edges_and_marks_no_output() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(chain(e1)).await;
    let mut before = case.rows().await;
    let paths_of_chain = before.remove("cached_path").unwrap_or_default();

    let report = case.commit_nar(nar(SOURCE, "source")).await;

    let stored = case.cached_path(SOURCE).await;
    let report = (report.created, report.outputs_marked);
    assert_eq!(report, (true, 0), "after 1: report");
    let mut after = case.rows().await;
    let paths = after.remove("cached_path").unwrap_or_default();
    assert_eq!(after, before, "after 1: CHAIN rows and edges unchanged");
    let stored_before = paths_of_chain.len();
    assert_eq!(paths.len(), stored_before + 1, "after 1: {paths:?}");
    let all = paths_of_chain.iter().all(|row| paths.contains(row));
    assert!(all, "after 1: the cached paths of CHAIN are unchanged");
    case.zero_drift("step 1").await;

    let report = case.commit_nar(nar(SOURCE, "source")).await;

    assert!(!report.created, "after 2: report.created");
    let row = case.cached_path(SOURCE).await;
    assert_eq!(row, stored, "after 2: row unchanged");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn the_cached_path_row_is_read_under_a_row_lock() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(chain_done(e1)).await;
    let second_connection = case.db.connection();
    let locking = second_connection
        .begin()
        .await
        .expect("a transaction on a second connection");

    cached_path::Entity::find()
        .filter(cached_path::Column::Hash.eq(B_OUT))
        .lock(LockType::NoKeyUpdate)
        .one(&locking)
        .await
        .expect("the row lock is taken")
        .expect("the row of B_OUT");

    let report = {
        let commit = case.commit_nar(commit_of(B_OUT, "b", &[(B_OUT, "b")]));
        tokio::pin!(commit);
        let waiting = tokio::time::timeout(Duration::from_millis(200), &mut commit).await;
        assert!(waiting.is_err(), "the commit passed the lock: {waiting:?}");

        locking.commit().await.expect("the row lock is released");
        commit.await
    };

    assert!(!report.created, "the commit read the locked row");
    case.zero_drift("the commit").await;
    case.finish().await;
}

#[tokio::test]
async fn a_commit_stores_the_content_address_of_its_path() {
    let case = Case::start(shape()).await;
    let ca = "text:sha256:006vc8gixyrcynsx4lz1qxingl0mdja3l0xw1nl0j73isg37x944";

    case.commit_nar(NarCommit {
        ca: Some(ca.to_owned()),
        ..nar(SOURCE, "source")
    })
    .await;

    let stored = case.cached_path(SOURCE).await.ca;
    assert_eq!(stored.as_deref(), Some(ca), "cached_path(SOURCE).ca");
    case.zero_drift("the commit").await;
    case.finish().await;
}

#[tokio::test]
async fn a_commit_that_is_not_confirmed_stores_an_unconfirmed_row() {
    let case = Case::start(shape()).await;

    case.commit_nar(NarCommit {
        confirmed: false,
        ..nar(SOURCE, "source")
    })
    .await;

    let row = case.cached_path(SOURCE).await;
    assert!(!row.confirmed, "cached_path(SOURCE).confirmed: {row:?}");
    assert!(row.file_hash.is_some(), "the row is stored: {row:?}");
    case.zero_drift("the commit").await;
    case.finish().await;
}
