/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus::{Building, Completed, Created, Queued, Substituted};
use gradient_entity::build_attempt::AttemptOutcome;
use gradient_entity::cached_path_signature;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::{CacheId, EvaluationId};
use gradient_fixtures::shape::shape;

use super::{
    A, A_OUT, B, B_OUT, C, C_OUT, Case, D, D_OUT, F_OUT, F1, F2, P, P_OUT, UNKNOWN_OUT, X, X_OUT,
    as_set, chain, chain_done, commit_of, drv_nar, pass,
};
use crate::messages::{Demotion, NarCommit};

fn missing_nar(hash: &str) -> Demotion {
    Demotion::MissingNar {
        hash: hash.to_owned(),
    }
}

fn path_demotion(hash: &str) -> Demotion {
    Demotion::Path {
        hash: hash.to_owned(),
    }
}

fn drv_file(drv: &str) -> Demotion {
    let (hash, _) = drv.split_once('-').expect("a drv path");
    path_demotion(hash)
}

async fn rebuild(case: &Case, path: &str, commit: NarCommit) {
    case.build_started(path).await;
    case.commit_nar(commit).await;
    case.build_completed(path).await;
}

async fn state_of_the_chain(case: &Case, evaluation: EvaluationId) -> Vec<String> {
    let mut state = Vec::new();
    for path in [A, B] {
        let build = case.sb(path).await;
        let queue = (build.status, build.cache_available, build.probed);
        let closure = (build.fetchable, build.missing_runtime_deps);
        let cached = case.output_stored(path, "out").await;
        let pending = (build.blocking_deps, build.attempt);
        let columns = format!("{queue:?} {closure:?} {pending:?} {cached}");
        state.push(format!("{path}: {columns}"));
    }

    let rows = case.rows().await;
    for table in [
        "derivation",
        "derivation_dependency",
        "build_job",
        "entry_point",
        "build_attempt",
    ] {
        state.push(format!("{table}: {:?}", rows[table]));
    }

    state.push(format!("sb(A).wanted: {}", case.sb(A).await.wanted));
    state.push(format!("counters: {:?}", case.counters(evaluation).await));
    state
}

#[tokio::test]
async fn r01_retiring_an_output_resets_the_producer_and_the_recommit_restores_every_counter() {
    for (variant, demotion) in [
        ("MissingNar", missing_nar(B_OUT)),
        ("Path", path_demotion(B_OUT)),
    ] {
        let (e1, c1) = (EvaluationId::now_v7(), CacheId::now_v7());
        let case = Case::start(chain_done(e1).into_cache(c1).keyed(Case::key())).await;
        case.seed_signature(B_OUT, c1, Some(vec![7; 64])).await;
        let start = state_of_the_chain(&case, e1).await;

        let report = case.demote(demotion).await;

        let stored = case.find_cached_path(B_OUT).await;
        assert_eq!(stored, None, "{variant} after 1: cached_path(B_OUT)");
        let signatures = case.db.rows::<cached_path_signature::Entity>().await;
        let none = signatures.is_empty();
        assert!(none, "{variant} after 1: {signatures:?}");
        let b = case.sb(B).await;
        let queue = (b.status, b.wanted, b.cache_available);
        assert_eq!(queue, (Queued, true, false), "{variant} after 1: sb(B)");
        let closure = (b.fetchable, b.missing_runtime_deps);
        assert_eq!(closure, (false, 0), "{variant} after 1: sb(B)");
        let a = case.queue_state(A).await;
        assert_eq!(a, (Created, 1), "{variant} after 1: sb(A)");
        let producers = as_set(&report.producers);
        let only_b = case.ids(&[B]).await;
        assert_eq!(producers, only_b, "{variant} after 1: report.producers");
        let counters = case.counters(e1).await;
        assert_eq!(counters, [2, 2, 0, 1, 0], "{variant} after 1: counters");
        case.zero_drift("step 1").await;

        let commit = NarCommit {
            targets: case.project_caches(e1).await,
            ..commit_of(B_OUT, "b", &[(B_OUT, "b")])
        };

        rebuild(&case, B, commit).await;

        let b = case.sb(B).await;
        let b = (b.status, b.fetchable);
        assert_eq!(b, (Completed, true), "{variant} after 2: sb(B)");
        let a = case.queue_state(A).await;
        assert_eq!(a, (Queued, 0), "{variant} after 2: sb(A)");
        let signature = case.signature(B_OUT, c1).await;
        let signed = signature.is_some_and(|row| row.signature.is_some());
        assert!(signed, "{variant} after 2: signature(B_OUT, C1)");
        let counters = case.counters(e1).await;
        assert_eq!(counters, [2, 1, 0, 1, 0], "{variant} after 2: counters");
        let end = state_of_the_chain(&case, e1).await;
        assert_eq!(end, start, "{variant} after 2: the CHAIN_DONE start");
        case.zero_drift("step 2").await;
        case.finish().await;
    }
}

#[tokio::test]
async fn r02_a_build_that_only_referenced_the_path_stays_terminal_and_loses_its_closure() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        shape()
            .evaluation(e1)
            .derivation(X)
            .walked()
            .entry_point("pkg.x")
            .job()
            .probed()
            .drv_cached()
            .output(X_OUT)
            .depends_on(B)
            .build_status(Queued)
            .derivation(B)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .build_status(Completed)
            .cached_output(B)
            .references(&[(B_OUT, "b"), (D_OUT, "d")])
            .runtime_depends_on(D)
            .derivation(D)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(D_OUT)
            .build_status(Completed)
            .cached_output(D),
    )
    .await;

    case.demote(missing_nar(D_OUT)).await;

    let d = case.sb(D).await;
    let d = (d.status, d.fetchable);
    assert_eq!(d, (Queued, false), "after 1: sb(D)");
    let b = case.sb(B).await;
    let b = (b.status, b.missing_runtime_deps, b.fetchable, b.wanted);
    assert_eq!(b, (Completed, 1, false, true), "after 1: sb(B)");
    let x = case.queue_state(X).await;
    assert_eq!(x, (Created, 1), "after 1: sb(X)");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [3, 2, 0, 1, 0], "after 1: counters(E1)");
    case.zero_drift("step 1").await;

    rebuild(&case, D, commit_of(D_OUT, "d", &[(D_OUT, "d")])).await;

    assert!(case.sb(D).await.fetchable, "after 2: sb(D).fetchable");
    let b = case.closure_state(B).await;
    assert_eq!(b, (true, 0), "after 2: sb(B) fetchable, missing 0");
    let x = case.queue_state(X).await;
    assert_eq!(x, (Queued, 0), "after 2: sb(X)");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn r03_retiring_a_drv_unqueues_its_queued_owner_and_the_recommit_queues_it_again() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        chain(e1)
            .derivation(C)
            .walked()
            .entry_point("pkg.c")
            .job()
            .probed()
            .drv_cached()
            .output(C_OUT)
            .build_status(Building)
            .attempt(AttemptOutcome::Running, None),
    )
    .await;

    case.demote(drv_file(B)).await;
    case.demote(drv_file(C)).await;

    let b = case.sb(B).await;
    let b = (b.status, b.wanted);
    assert_eq!(b, (Created, true), "after 1: sb(B)");
    let c = case.sb(C).await.status;
    assert_eq!(c, Building, "after 1: sb(C).status");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [3, 3, 0, 0, 1], "after 1: counters(E1)");
    case.zero_drift("step 1").await;

    case.commit_nar(drv_nar(B)).await;

    let b = case.sb(B).await.status;
    assert_eq!(b, Queued, "after 2: sb(B).status");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn r04_retiring_the_output_of_a_substituted_passthrough_makes_it_a_builder() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        pass(e1)
            .derivation(A)
            .build_status(Substituted)
            .cached_output(A)
            .evaluation_status(EvaluationStatus::Completed),
    )
    .await;

    case.demote(missing_nar(A_OUT)).await;

    let a = case.queue_state(A).await;
    assert_eq!(a, (Created, 1), "after 1: sb(A) status, blocking_deps");
    let a = case.sb(A).await;
    let a = (a.cache_available, a.probed, a.wanted);
    assert_eq!(a, (false, true, true), "after 1: sb(A)");
    let b = case.sb(B).await;
    let b = (b.status, b.attempt, b.wanted);
    assert_eq!(b, (Queued, 0, true), "after 1: sb(B)");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 2, 0, 1, 0], "after 1: counters(E1)");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn r05_a_cache_claim_deletes_only_its_row_and_the_last_claim_retires_the_path() {
    let (e1, c1, c2) = (EvaluationId::now_v7(), CacheId::now_v7(), CacheId::now_v7());
    let case = Case::start(
        chain_done(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .into_cache(c2)
            .keyed(Case::key()),
    )
    .await;

    case.seed_signature(B_OUT, c1, Some(vec![7; 64])).await;
    case.seed_signature(B_OUT, c2, Some(vec![8; 64])).await;
    let claim = |cache, hash: &str| Demotion::CacheClaim {
        cache,
        hash: hash.to_owned(),
    };

    let report = case.demote(claim(c1, B_OUT)).await;

    let signature = case.signature(B_OUT, c1).await;
    assert_eq!(signature, None, "after 1: signature(B_OUT, C1)");
    let stored = case.output_stored(B, "out").await;
    assert!(stored, "after 1: cached_path(output(B,out).hash) stored");
    assert!(case.sb(B).await.fetchable, "after 1: sb(B).fetchable");
    assert!(report.others_remain, "after 1: report.others_remain");
    case.zero_drift("step 1").await;

    let report = case.demote(claim(c2, B_OUT)).await;

    let stored = case.find_cached_path(B_OUT).await;
    assert_eq!(stored, None, "after 2: cached_path(B_OUT)");
    let b = case.sb(B).await;
    let b = (b.status, b.fetchable);
    assert_eq!(b, (Queued, false), "after 2: sb(B)");
    let a = case.queue_state(A).await;
    assert_eq!(a, (Created, 1), "after 2: sb(A)");
    assert!(!report.others_remain, "after 2: report.others_remain");
    case.zero_drift("step 2").await;
    let before = case.rows().await;

    let report = case.demote(claim(c2, UNKNOWN_OUT)).await;

    let reported = report.cached_path;
    assert_eq!(reported, None, "after 3: report.cached_path");
    assert!(!report.others_remain, "after 3: report.others_remain");
    assert_eq!(case.rows().await, before, "after 3: nothing changed");
    case.zero_drift("step 3").await;
    case.finish().await;
}

#[tokio::test]
async fn r06_retiring_a_path_with_two_producers_resets_both() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
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
            .cached_output(F1)
            .derivation(F2)
            .walked()
            .entry_point("pkg.f2")
            .job()
            .probed()
            .drv_cached()
            .output(F_OUT)
            .build_status(Substituted)
            .cached_output(F2),
    )
    .await;

    let report = case.demote(missing_nar(F_OUT)).await;

    let f1 = case.sb(F1).await;
    let f1 = (f1.status, f1.fetchable);
    assert_eq!(f1, (Queued, false), "after 1: sb(F1)");
    let f2 = case.sb(F2).await;
    let f2 = (f2.status, f2.fetchable, f2.cache_available);
    assert_eq!(f2, (Queued, false, false), "after 1: sb(F2)");
    let producers = as_set(&report.producers);
    let both = case.ids(&[F1, F2]).await;
    assert_eq!(producers, both, "after 1: report.producers");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 2, 0, 2, 0], "after 1: counters(E1)");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn r07_a_building_parent_outlasts_the_retire_of_its_input_and_ends_incomplete() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        shape()
            .evaluation(e1)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(A)
            .walked()
            .entry_point("pkg.a")
            .job()
            .probed()
            .drv_cached()
            .output(A_OUT)
            .both_depends_on(B)
            .build_status(Building)
            .attempt(AttemptOutcome::Running, None)
            .open_assignment()
            .derivation(B)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .build_status(Completed)
            .cached_output(B),
    )
    .await;

    case.demote(missing_nar(B_OUT)).await;

    let a = case.sb(A).await;
    let a = (a.status, a.blocking_deps, a.missing_runtime_deps);
    assert_eq!(a, (Building, 1, 1), "after 1: sb(A)");
    let b = case.sb(B).await.status;
    assert_eq!(b, Queued, "after 1: sb(B)");
    case.zero_drift("step 1").await;

    case.commit_nar(commit_of(A_OUT, "a", &[(A_OUT, "a"), (B_OUT, "b")]))
        .await;

    case.build_completed(A).await;

    let a = case.sb(A).await;
    let a = (a.status, a.fetchable, a.wanted);
    assert_eq!(a, (Completed, false, true), "after 2: sb(A)");
    let status = case.evaluation(e1).await.status;
    let building = EvaluationStatus::Building;
    assert_eq!(status, building, "after 2: evaluation(E1).status");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 1, 0, 1, 0], "after 2: counters(E1)");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn r08_a_three_level_runtime_chain_reverses_exactly() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        shape()
            .evaluation(e1)
            .derivation(P)
            .walked()
            .entry_point("pkg.p")
            .job()
            .probed()
            .drv_cached()
            .output(P_OUT)
            .depends_on(X)
            .build_status(Queued)
            .derivation(X)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(X_OUT)
            .build_status(Completed)
            .cached_output(X)
            .runtime_depends_on(A)
            .derivation(A)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(A_OUT)
            .build_status(Completed)
            .cached_output(A)
            .runtime_depends_on(B)
            .derivation(B)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .build_status(Completed)
            .cached_output(B),
    )
    .await;

    case.demote(missing_nar(B_OUT)).await;

    for (path, missing) in [(X, 1), (A, 1), (B, 0)] {
        let closure = case.closure_state(path).await;
        assert_eq!(closure, (false, missing), "after 1: {path}");
    }

    let b = case.sb(B).await.status;
    assert_eq!(b, Queued, "after 1: sb(B)");
    let p = case.queue_state(P).await;
    assert_eq!(p, (Created, 1), "after 1: sb(P)");
    case.zero_drift("step 1").await;
    let after_first = case.rows().await;

    let report = case.demote(missing_nar(B_OUT)).await;

    let rows = case.rows().await;
    assert_eq!(rows, after_first, "after 2: identical to after 1");
    let producers = as_set(&report.producers);
    let only_b = case.ids(&[B]).await;
    assert_eq!(producers, only_b, "after 2: producers, weak spot 6");
    case.zero_drift("step 2").await;

    rebuild(&case, B, commit_of(B_OUT, "b", &[(B_OUT, "b")])).await;

    for path in [X, A, B] {
        let closure = case.closure_state(path).await;
        assert_eq!(closure, (true, 0), "after 3: {path}");
    }

    let p = case.queue_state(P).await;
    assert_eq!(p, (Queued, 0), "after 3: sb(P)");
    case.zero_drift("step 3").await;
    case.finish().await;
}

#[tokio::test]
async fn r09_a_retire_while_the_producer_is_building_leaves_its_attempt_alone() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        chain_done(e1)
            .derivation(B)
            .build_status(Building)
            .attempt(AttemptOutcome::Running, None)
            .open_assignment()
            .derivation(A)
            .build_status(Created),
    )
    .await;

    case.demote(missing_nar(B_OUT)).await;

    let b = case.sb(B).await.status;
    assert_eq!(b, Building, "after 1: sb(B).status");
    let outcome = case.latest_attempt(B).await.outcome;
    let running = AttemptOutcome::Running;
    assert_eq!(outcome, running, "after 1: attempt(B, latest).outcome");
    let stored = case.output_stored(B, "out").await;
    assert!(!stored, "after 1: cached_path(output(B,out).hash) stored");
    let a = case.queue_state(A).await;
    assert_eq!(a, (Created, 1), "after 1: sb(A)");
    case.zero_drift("step 1").await;
    case.finish().await;
}
