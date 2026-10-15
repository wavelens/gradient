/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeSet;

use chrono::{Duration, NaiveDateTime};
use gradient_db::maintenance::gc::expired_cached_paths;
use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{self, AttemptOutcome};
use gradient_entity::derivation_build;
use gradient_entity::ids::EvaluationId;
use gradient_fixtures::shape::shape;

use super::{
    A, A_OUT, B, B_OUT, C, C_OUT, Case, D, F, F_OUT, N, N_OUT, O, O_OUT, O2, O2_OUT, X, X_OUT, Y,
    Y_OUT, as_set, chain, chain_done, diamond, drv, nar, t0, with_attr, with_output,
    with_references,
};
use crate::messages::{GcReport, GcRequest};

fn older_than_the_scan() -> NaiveDateTime {
    t0() - Duration::hours(48)
}

fn drv_hash(path: &str) -> &str {
    path.split_once('-').map_or(path, |(hash, _)| hash)
}

async fn gc_derivations(case: &Case, candidates: &[&str]) -> GcReport {
    case.gc(GcRequest::Derivations {
        candidates: case.ids(candidates).await.into_iter().collect(),
    })
    .await
}

async fn gc_paths(case: &Case, hashes: impl IntoIterator<Item = String>) -> GcReport {
    case.gc(GcRequest::Paths {
        hashes: hashes.into_iter().collect(),
        scanned_at: t0(),
    })
    .await
}

async fn gc_evaluations(case: &Case, ids: &[EvaluationId]) -> GcReport {
    case.gc(GcRequest::Evaluations { ids: ids.to_vec() }).await
}

// Q23, Q24: the scan alone can tell a live path, so C05 and C07 pass its list to the writer.
async fn paths_the_scan_lists(case: &Case) -> BTreeSet<String> {
    let keep_hours = 0;
    expired_cached_paths(&case.ctx.worker_db, keep_hours)
        .await
        .expect("the scan runs")
        .into_iter()
        .collect()
}

async fn stored(case: &Case, hash: &str) -> bool {
    case.find_cached_path(hash).await.is_some()
}

#[tokio::test]
async fn c01_derivation_gc_deletes_a_candidate_nobody_names_and_leaves_its_cached_path() {
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
            .build_status(BuildStatus::Queued)
            .derivation(O)
            .walked()
            .probed()
            .drv_cached()
            .output(O_OUT)
            .build_status(BuildStatus::Completed)
            .cached_output(O)
            .attempt(AttemptOutcome::Built, None)
            .created_at(older_than_the_scan()),
    )
    .await;

    let (derivation, shared_build) = (case.db.id_of(O).await, case.sb(O).await.id);
    let attempt = case.latest_attempt(O).await.id;
    let build_of_a = case.sb(A).await;

    let report = gc_derivations(&case, &[O]).await;

    let deleted = case.find_derivation(O).await.is_none();
    assert!(deleted, "after 1: derivation(O) gone");
    let build = case.db.find::<derivation_build::Entity>(shared_build).await;
    assert!(build.is_none(), "after 1: sb(O) gone: {build:?}");
    let attempts = case.db.rows::<build_attempt::Entity>().await;
    assert!(attempts.is_empty(), "after 1: attempt rows of O gone");
    let expected = GcReport {
        deleted_derivations: vec![derivation],
        attempt_logs: vec![attempt],
        ..Default::default()
    };
    assert_eq!(report, expected, "after 1: report");
    let path = stored(&case, O_OUT).await;
    assert!(path, "after 1: cached_path(O_OUT) present");
    assert_eq!(case.sb(A).await, build_of_a, "after 1: sb(A) unchanged");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [1, 1, 0, 1, 0], "after 1: counters(E1)");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn c02_a_candidate_named_through_a_job_created_since_the_scan_stays() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        shape()
            .evaluation(e1)
            .derivation(O)
            .walked()
            .probed()
            .drv_cached()
            .output(O_OUT)
            .depends_on(O2)
            .build_status(BuildStatus::Skipped)
            .created_at(older_than_the_scan())
            .derivation(O2)
            .walked()
            .probed()
            .drv_cached()
            .output(O2_OUT)
            .build_status(BuildStatus::Skipped)
            .created_at(older_than_the_scan()),
    )
    .await;

    let above_o = with_attr(with_output(drv(X, &[O]), X_OUT), "pkg.x");
    case.walk(case.tasked(e1, vec![above_o]).await).await;
    case.zero_drift("step 1").await;

    let report = gc_derivations(&case, &[O, O2]).await;

    for path in [O, O2] {
        let present = case.find_derivation(path).await.is_some();
        assert!(present, "after 2: derivation({path}) present");
    }
    let deleted = as_set(&report.deleted_derivations);
    assert!(deleted.is_empty(), "after 2: report.deleted: {deleted:?}");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn c03_a_listed_candidate_still_named_or_referenced_stays_and_only_the_orphan_goes() {
    let e1 = EvaluationId::now_v7();
    let completed_and_old = |shape: gradient_fixtures::shape::Shape, path, out| {
        shape
            .derivation(path)
            .walked()
            .probed()
            .drv_cached()
            .output(out)
            .build_status(BuildStatus::Completed)
            .cached_output(path)
            .created_at(older_than_the_scan())
    };
    let seed = shape()
        .evaluation(e1)
        .derivation(A)
        .walked()
        .entry_point("pkg.a")
        .job()
        .probed()
        .drv_cached()
        .output(A_OUT)
        .depends_on(B)
        .runtime_depends_on(O)
        .derivation(B)
        .walked()
        .job()
        .probed()
        .drv_cached()
        .output(B_OUT)
        .build_status(BuildStatus::Queued)
        .created_at(older_than_the_scan());
    let seed = completed_and_old(seed, O, O_OUT).depends_on(O2);
    let seed = completed_and_old(completed_and_old(seed, O2, O2_OUT), Y, Y_OUT);
    let case = Case::start(seed).await;
    let (build_of_a, counters) = (case.sb(A).await, case.counters(e1).await);
    let orphan = case.db.id_of(Y).await;

    let report = gc_derivations(&case, &[B, O, O2, Y]).await;

    for path in [B, O, O2] {
        let present = case.find_derivation(path).await.is_some();
        assert!(present, "after 1: derivation({path}) present");
    }
    let gone = case.find_derivation(Y).await.is_none();
    assert!(gone, "after 1: derivation(Y) gone");
    assert_eq!(
        report.deleted_derivations,
        vec![orphan],
        "after 1: report.deleted"
    );
    assert_eq!(case.sb(A).await, build_of_a, "after 1: sb(A) unchanged");
    assert_eq!(
        case.counters(e1).await,
        counters,
        "after 1: counters(E1) unchanged"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn c04_an_empty_candidate_list_changes_nothing() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(chain(e1)).await;
    let (rows, gv) = (case.rows().await, case.gv(e1).await);

    let report = gc_derivations(&case, &[]).await;

    assert_eq!(case.rows().await, rows, "after 1: all rows unchanged");
    let deleted = as_set(&report.deleted_derivations);
    assert!(deleted.is_empty(), "after 1: report.deleted: {deleted:?}");
    assert_eq!(case.gv(e1).await, gv, "after 1: gv(E1) unchanged");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn c05_path_gc_retires_only_what_the_scan_lists_as_not_live() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        chain_done(e1)
            .derivation(O)
            .walked()
            .probed()
            .drv_cached()
            .output(O_OUT)
            .build_status(BuildStatus::Completed)
            .cached_output(O),
    )
    .await;

    let listed = paths_the_scan_lists(&case).await;
    gc_paths(&case, listed.clone()).await;

    let not_live = BTreeSet::from([O_OUT.to_owned(), drv_hash(O).to_owned()]);
    assert_eq!(listed, not_live, "the scan lists O_OUT and O.drv only");
    for (hash, present) in [
        (B_OUT, true),
        (drv_hash(B), true),
        (O_OUT, false),
        (drv_hash(O), false),
    ] {
        let path = stored(&case, hash).await;
        assert_eq!(path, present, "after 1: cached_path({hash}) present");
    }
    let producer = case.sb(O).await;
    let state = (producer.status, producer.fetchable);
    assert_eq!(state, (BuildStatus::Skipped, false), "after 1: sb(O)");
    let fetchable = case.sb(B).await.fetchable;
    assert!(fetchable, "after 1: sb(B).fetchable");
    let status = case.sb(A).await.status;
    assert_eq!(status, BuildStatus::Queued, "after 1: sb(A).status");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn c06_a_path_of_an_output_row_created_since_the_scan_stays() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1).cached_path(N_OUT, "n")).await;

    let producer = with_attr(with_output(drv(N, &[]), N_OUT), "pkg.n");
    case.walk(case.tasked(e1, vec![producer]).await).await;
    case.zero_drift("step 1").await;

    gc_paths(&case, [N_OUT.to_owned()]).await;

    let path = case.output_stored(N, "out").await;
    assert!(path, "after 2: cached_path(output(N,out).hash) stored");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn c07_the_output_of_a_build_input_of_a_pending_named_parent_is_live() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(chain_done(e1)).await;

    let listed = paths_the_scan_lists(&case).await;
    gc_paths(&case, listed.clone()).await;

    let live = !listed.contains(B_OUT);
    assert!(live, "the scan does not list B_OUT: {listed:?}");
    let path = stored(&case, B_OUT).await;
    assert!(path, "after 1: cached_path(B_OUT) present");
    let status = case.sb(A).await.status;
    assert_eq!(status, BuildStatus::Queued, "after 1: sb(A).status");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn c08_evaluation_gc_cascades_jobs_and_a_live_evaluation_adopts_the_open_builds() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
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
            .depends_on(A)
            .depends_on(F)
            .derivation(A)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(A_OUT)
            .build_status(BuildStatus::Queued)
            .derivation(F)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(F_OUT)
            .build_status(BuildStatus::Completed)
            .cached_output(F)
            .evaluation(e2)
            .derivation(Y)
            .walked()
            .entry_point("pkg.y")
            .job()
            .probed()
            .drv_cached()
            .output(Y_OUT)
            .depends_on(X)
            .derivation(X)
            .job(),
    )
    .await;

    let gv = case.gv(e2).await;

    gc_evaluations(&case, &[e1]).await;

    let adopted = case.job(e2, A).await.is_some();
    assert!(adopted, "after 1: build_job(E2,A) exists");
    let fetchable_named = case.job(e2, F).await;
    assert!(fetchable_named.is_none(), "after 1: no build_job(E2,F)");
    let open = case.sb(A).await;
    let state = (open.status, open.wanted);
    assert_eq!(state, (BuildStatus::Queued, true), "after 1: sb(A)");
    let done = case.sb(F).await;
    let state = (done.status, done.fetchable);
    assert_eq!(state, (BuildStatus::Completed, true), "after 1: sb(F)");
    assert_eq!(case.gv(e2).await, gv + 1, "after 1: gv(E2) +1");
    let counters = case.counters(e2).await;
    assert_eq!(counters, [3, 3, 0, 1, 0], "after 1: counters(E2)");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn c09_without_an_adopter_a_queued_build_is_skipped_and_a_building_build_finishes() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        diamond(e1)
            .derivation(C)
            .build_status(BuildStatus::Building)
            .attempt(AttemptOutcome::Running, None),
    )
    .await;

    let job = case.latest_attempt(C).await.build_job;
    assert!(job.is_some(), "given: the attempt of C names its job");

    gc_evaluations(&case, &[e1]).await;

    let queued = case.sb(D).await;
    let state = (queued.status, queued.wanted);
    assert_eq!(state, (BuildStatus::Skipped, false), "after 1: sb(D)");
    let status = case.sb(C).await.status;
    assert_eq!(status, BuildStatus::Building, "after 1: sb(C)");
    for path in [A, B] {
        let status = case.sb(path).await.status;
        assert_eq!(status, BuildStatus::Skipped, "after 1: sb({path})");
    }
    let job = case.latest_attempt(C).await.build_job;
    assert_eq!(job, None, "after 1: attempt(C, latest).build_job");
    case.zero_drift("step 1").await;

    case.commit_nar(with_references(nar(C_OUT, "c"), &[(C_OUT, "c")]))
        .await;
    case.build_completed(C).await;

    let built = case.sb(C).await;
    let state = (built.status, built.fetchable);
    assert_eq!(state, (BuildStatus::Completed, true), "after 2: sb(C)");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn c10_evaluation_gc_leaves_the_other_sharing_evaluation_complete() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = Case::start(
        chain(e1)
            .evaluation(e2)
            .derivation(A)
            .entry_point("pkg.a")
            .job()
            .derivation(B)
            .job(),
    )
    .await;

    let gv = case.gv(e2).await;

    gc_evaluations(&case, &[e1]).await;

    for path in [A, B] {
        let named = case.job(e2, path).await.is_some();
        assert!(named, "after 1: build_job(E2,{path}) exists");
    }
    let queued = case.sb(B).await;
    let state = (queued.status, queued.wanted);
    assert_eq!(state, (BuildStatus::Queued, true), "after 1: sb(B)");
    let counters = case.counters(e2).await;
    assert_eq!(counters, [2, 2, 0, 1, 0], "after 1: counters(E2)");
    assert_eq!(case.gv(e2).await, gv, "after 1: gv(E2) unchanged");
    case.zero_drift("step 1").await;
    case.finish().await;
}
