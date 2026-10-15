/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus::{self, Completed, Created, Queued, Skipped, Substituted};
use gradient_entity::derivation_dependency::EdgeKind;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::EvaluationId;
use gradient_fixtures::shape::shape;
use gradient_wire::types::DiscoveredDerivation;

use super::{
    A, A_OUT, B, B_OUT, C, C_OUT, Case, D, F_OUT, F1, F2, X, X_OUT, Y, Y_OUT, Z, Z_OUT, as_set,
    batch, chain, chain_done, drv, drv_nar, with_attr, with_ifd, with_limits, with_output,
};
use crate::messages::Transition;
use crate::test_ctx::{Batch, BatchReport};

#[derive(Debug, Default, PartialEq)]
struct SharedBuild {
    status: BuildStatus,
    cache_available: bool,
    probed: bool,
    wanted: bool,
    blocking_deps: i32,
    missing_runtime_deps: i32,
    fetchable: bool,
    attempt: i32,
}

async fn shared_build(case: &Case, path: &str) -> SharedBuild {
    let row = case.sb(path).await;
    SharedBuild {
        status: row.status,
        cache_available: row.cache_available,
        probed: row.probed,
        wanted: row.wanted,
        blocking_deps: row.blocking_deps,
        missing_runtime_deps: row.missing_runtime_deps,
        fetchable: row.fetchable,
        attempt: row.attempt,
    }
}

async fn named(case: &Case, evaluation: EvaluationId, path: &str) -> bool {
    case.job(evaluation, path).await.is_some()
}

async fn unwalked_inputs(case: &Case, path: &str) -> i32 {
    case.derivation(path).await.unwalked_inputs
}

async fn walk(
    case: &Case,
    evaluation: EvaluationId,
    derivations: Vec<DiscoveredDerivation>,
) -> BatchReport {
    case.walk(case.tasked(evaluation, derivations).await).await
}

fn walked(path: &str, output: &str, dependencies: &[&str]) -> DiscoveredDerivation {
    with_output(drv(path, dependencies), output)
}

fn a_above_b() -> DiscoveredDerivation {
    with_attr(walked(A, A_OUT, &[B]), "pkg.a")
}

async fn a_above_a_walked_b(case: &Case, evaluation: EvaluationId) -> Batch {
    let derivations = vec![a_above_b(), walked(B, B_OUT, &[])];
    case.tasked(evaluation, derivations).await
}

#[tokio::test]
async fn b01_a_first_batch_writes_its_graph_rows_and_leaves_the_unwanted_stub_skipped() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;
    let gv = case.gv(e1).await;

    let report = walk(&case, e1, vec![a_above_b()]).await;

    let (a, b) = (case.derivation(A).await, case.derivation(B).await);
    assert_eq!((a.walked, a.unwalked_inputs), (true, 1), "after 1: A");
    assert_eq!((b.walked, b.unwalked_inputs), (false, 0), "after 1: B");
    let kind = case.edge_kind(A, B).await;
    assert_eq!(kind, Some(EdgeKind::Buildtime), "after 1: edge(A,B).kind");
    let output = case.output_of(A, "out").await;
    assert_eq!(output.hash, A_OUT, "after 1: output(A,out).hash");
    let columns = (output.is_cached, output.external_url);
    assert_eq!(columns, (false, None), "after 1: output(A,out)");
    let created = SharedBuild {
        wanted: true,
        blocking_deps: 1,
        ..Default::default()
    };

    assert_eq!(shared_build(&case, A).await, created, "after 1: sb(A)");
    for path in [A, B] {
        let aborted = case.job(e1, path).await.map(|job| job.aborted);
        assert_eq!(aborted, Some(false), "after 1: job of E1 on {path}");
    }

    let entry_point = case.entry_point(e1, "pkg.a").await;
    let derivation = entry_point.map(|row| row.derivation);
    assert_eq!(derivation, Some(a.id), "after 1: entry_point(E1, pkg.a)");
    let columns = (report.walked, report.skipped);
    assert_eq!(columns, (1, false), "after 1: report");
    let asked = case.ids(&[A]).await;
    assert_eq!(as_set(&report.to_probe), asked, "after 1: report.to_probe");
    assert_eq!(case.gv(e1).await, gv + 3, "after 1: gv(E1)");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 1, 0, 0, 0], "after 1: counters(E1)");
    let skipped = SharedBuild {
        status: Skipped,
        ..Default::default()
    };

    assert_eq!(shared_build(&case, B).await, skipped, "after 1: sb(B)");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn b02_a_batch_outside_the_evaluating_statuses_is_skipped_and_fetching_is_accepted() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = Case::start(
        shape()
            .evaluation(e1)
            .evaluation_status(EvaluationStatus::Completed)
            .evaluation(e2)
            .evaluation_status(EvaluationStatus::Fetching),
    )
    .await;

    let entry_point = || vec![with_attr(walked(A, A_OUT, &[]), "pkg.a")];
    let gv = case.gv(e1).await;

    let report = walk(&case, e1, entry_point()).await;

    assert!(report.skipped, "after 1: report.skipped");
    let a = case.find_derivation(A).await;
    assert_eq!(a, None, "after 1: derivation(A)");
    let row = case.entry_point(e1, "pkg.a").await;
    assert_eq!(row, None, "after 1: entry_point(E1, pkg.a)");
    assert_eq!(case.gv(e1).await, gv, "after 1: gv(E1)");
    case.zero_drift("step 1").await;
    let gv = case.gv(e2).await;

    let report = walk(&case, e2, entry_point()).await;

    assert!(!report.skipped, "after 2: report.skipped");
    let a = case.derivation(A).await;
    assert!(a.walked, "after 2: derivation(A).walked");
    assert!(named(&case, e2, A).await, "after 2: build_job(E2,A)");
    assert_eq!(case.gv(e2).await, gv + 1, "after 2: gv(E2)");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn b03_walking_a_stub_marks_it_walked_and_drops_the_unwalked_inputs_of_its_parent() {
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
            .job(),
    )
    .await;

    let gv = case.gv(e1).await;

    let report = walk(&case, e1, vec![walked(B, B_OUT, &[])]).await;

    let b = case.derivation(B).await;
    assert!(b.walked, "after 1: derivation(B).walked");
    let inputs = unwalked_inputs(&case, A).await;
    assert_eq!(inputs, 0, "after 1: derivation(A).unwalked_inputs");
    assert_eq!(report.walked, 1, "after 1: report.walked");
    let asked = case.ids(&[B]).await;
    assert_eq!(as_set(&report.to_probe), asked, "after 1: report.to_probe");
    assert_eq!(case.probe_requests(), asked, "after 1: probe requests");
    let wanted = SharedBuild {
        wanted: true,
        ..Default::default()
    };

    assert_eq!(shared_build(&case, B).await, wanted, "after 1: sb(B)");
    let a = case.sb(A).await;
    let columns = (a.status, a.blocking_deps);
    assert_eq!(columns, (Created, 1), "after 1: sb(A)");
    let jobs = case.jobs(e1).await.len();
    assert_eq!(jobs, 2, "after 1: build jobs of E1");
    let known = case.known(&[A, B]).await;
    assert_eq!(known, [A, B], "after 1: known_derivations");
    assert_eq!(case.gv(e1).await, gv + 1, "after 1: gv(E1)");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 2, 0, 0, 0], "after 1: counters(E1)");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn b04_a_batch_delivered_twice_changes_only_graph_version() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;

    walk(&case, e1, vec![a_above_b()]).await;

    let (after_1, gv) = (case.rows().await, case.gv(e1).await);
    case.zero_drift("step 1").await;

    let report = walk(&case, e1, vec![a_above_b()]).await;

    let columns = (report.walked, report.skipped);
    assert_eq!(columns, (0, false), "after 2: report");
    assert_eq!(report.to_probe, vec![], "after 2: report.to_probe");
    let after_2 = case.rows().await;
    let tables = [
        "derivation",
        "derivation_dependency",
        "derivation_output",
        "build_job",
        "entry_point",
        "derivation_build",
    ];

    let rows = tables.map(|table| after_2[table].len());
    assert_eq!(rows, [2, 1, 1, 2, 1, 2], "after 2: rows of {tables:?}");
    assert_eq!(after_2, after_1, "after 2: every row as after 1");
    assert_eq!(case.gv(e1).await, gv + 1, "after 2: gv(E1)");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 1, 0, 0, 0], "after 2: counters(E1)");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn b05_a_build_edge_onto_a_runtime_edge_becomes_both_and_the_counters_include_it() {
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
            .both_depends_on(C)
            .depends_on(D)
            .derivation(B)
            .job()
            .derivation(C)
            .job()
            .derivation(D)
            .job(),
    )
    .await;

    let gv = case.gv(e1).await;
    let a = with_attr(walked(A, A_OUT, &[B, C, D]), "pkg.a");

    let report = walk(&case, e1, vec![a]).await;

    let to_b = case.edge_kind(A, B).await;
    assert_eq!(to_b, Some(EdgeKind::Both), "after 1: edge(A,B).kind");
    let to_c = case.edge_kind(A, C).await;
    assert_eq!(to_c, Some(EdgeKind::Both), "after 1: edge(A,C).kind");
    let to_d = case.edge_kind(A, D).await;
    assert_eq!(to_d, Some(EdgeKind::Buildtime), "after 1: edge(A,D).kind");
    let inputs = unwalked_inputs(&case, A).await;
    assert_eq!(inputs, 3, "after 1: derivation(A).unwalked_inputs");
    let a = case.sb(A).await;
    let columns = (a.blocking_deps, a.missing_runtime_deps, a.status);
    assert_eq!(columns, (3, 2, Created), "after 1: sb(A)");
    assert_eq!(report.walked, 0, "after 1: report.walked");
    assert_eq!(case.gv(e1).await, gv + 2, "after 1: gv(E1)");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [4, 4, 0, 0, 0], "after 1: counters(E1)");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn b06_a_grown_edge_set_raises_graph_version_of_every_naming_evaluation_once() {
    let (e1, e2, e3) = (
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
    );

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
            .derivation(A)
            .job()
            .evaluation(e2)
            .derivation(Y)
            .walked()
            .entry_point("pkg.y")
            .job()
            .probed()
            .drv_cached()
            .output(Y_OUT)
            .depends_on(A)
            .derivation(A)
            .job()
            .evaluation(e3)
            .derivation(Z)
            .walked()
            .entry_point("pkg.z")
            .job()
            .probed()
            .drv_cached()
            .output(Z_OUT)
            .build_status(Queued),
    )
    .await;

    let gv = [case.gv(e1).await, case.gv(e2).await, case.gv(e3).await];

    walk(&case, e1, vec![walked(A, A_OUT, &[B, C])]).await;

    let a = case.derivation(A).await;
    let columns = (a.walked, a.unwalked_inputs);
    assert_eq!(columns, (true, 2), "after 1: derivation(A)");
    for stub in [B, C] {
        let kind = case.edge_kind(A, stub).await;
        assert_eq!(kind, Some(EdgeKind::Buildtime), "after 1: edge(A, {stub})");
        let derivation = case.derivation(stub).await;
        assert!(!derivation.walked, "after 1: {stub} is a stub");
        assert!(named(&case, e1, stub).await, "after 1: E1 job on {stub}");
        assert!(!named(&case, e2, stub).await, "after 1: E2 job on {stub}");
    }

    let after = [case.gv(e1).await, case.gv(e2).await, case.gv(e3).await];
    let expected = [gv[0] + 3, gv[1] + 1, gv[2]];
    assert_eq!(after, expected, "after 1: gv of E1, E2, E3");
    let a = case.sb(A).await;
    let columns = (a.status, a.wanted, a.blocking_deps, a.probed);
    assert_eq!(columns, (Created, true, 2, false), "after 1: sb(A)");
    for parent in [X, Y] {
        let inputs = unwalked_inputs(&case, parent).await;
        assert_eq!(inputs, 1, "after 1: unwalked_inputs of {parent}");
    }

    let counters = [case.counters(e1).await, case.counters(e2).await];
    let expected = [[4, 2, 0, 0, 0], [2, 2, 0, 0, 0]];
    assert_eq!(counters, expected, "after 1: counters of E1, E2");
    for stub in [B, C] {
        let row = case.sb(stub).await;
        let columns = (row.status, row.wanted);
        assert_eq!(columns, (Skipped, false), "after 1: sb({stub})");
    }

    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn b07_a_dependency_that_is_no_store_path_fails_the_whole_batch() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;
    let gv = case.gv(e1).await;
    let c = || walked(C, C_OUT, &[]);
    let a = walked(A, A_OUT, &["/nix/store/not-a-store-path"]);
    let failing = case.tasked(e1, vec![with_attr(a, "pkg.a"), c()]).await;

    let result = case.walk_result(failing).await;

    assert!(result.is_err(), "after 1: result: {result:?}");
    let a = case.find_derivation(A).await;
    assert_eq!(a, None, "after 1: derivation(A)");
    let derivation = case.find_derivation(C).await;
    assert_eq!(derivation, None, "after 1: derivation(C)");
    let entry_point = case.entry_point(e1, "pkg.a").await;
    assert_eq!(entry_point, None, "after 1: entry_point(E1, pkg.a)");
    assert_eq!(case.gv(e1).await, gv, "after 1: gv(E1)");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [0; 5], "after 1: counters(E1)");
    let status = case.evaluation(e1).await.status;
    assert_eq!(status, EvaluationStatus::Failed, "after 1: status of E1");
    case.zero_drift("step 1").await;

    let report = walk(&case, e1, vec![c()]).await;

    assert!(report.skipped, "after 2: report.skipped");
    let derivation = case.find_derivation(C).await;
    assert_eq!(derivation, None, "after 2: derivation(C)");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn b08_the_limits_of_a_walked_derivation_are_written_onto_its_shared_build_only() {
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
            .job()
            .derivation(C)
            .job()
            .build_status(Skipped),
    )
    .await;

    case.override_sb(B, |row| {
        row.timeout_secs = Some(100);
        row.max_silent_secs = Some(50);
        row.prioritized = true;
    })
    .await;

    case.override_sb(C, |row| row.timeout_secs = Some(7)).await;
    let b = with_limits(walked(B, B_OUT, &[C]), 3600, 600);

    walk(&case, e1, vec![b]).await;

    let b = case.sb(B).await;
    let limits = (b.timeout_secs, b.max_silent_secs, b.prioritized);
    assert_eq!(limits, (Some(3600), Some(600), true), "after 1: sb(B)");
    let columns = (b.status, b.probed, b.wanted);
    assert_eq!(columns, (Created, false, true), "after 1: sb(B)");
    let c = case.sb(C).await;
    let columns = (c.timeout_secs, c.status, c.wanted);
    assert_eq!(columns, (Some(7), Skipped, false), "after 1: sb(C)");
    let inputs = unwalked_inputs(&case, A).await;
    assert_eq!(inputs, 1, "after 1: derivation(A).unwalked_inputs");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn b09_a_derivation_with_a_stored_output_starts_substituted_and_fetchable() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        shape()
            .evaluation(e1)
            .cached_path(B_OUT, "b")
            .references(&[(B_OUT, "b")]),
    )
    .await;

    let batch = a_above_a_walked_b(&case, e1).await;

    let report = case.walk(batch).await;

    let substituted = SharedBuild {
        status: Substituted,
        fetchable: true,
        ..Default::default()
    };

    assert_eq!(shared_build(&case, B).await, substituted, "after 1: sb(B)");
    let a = case.sb(A).await;
    let columns = (a.status, a.wanted, a.blocking_deps, a.probed);
    assert_eq!(columns, (Created, true, 0, false), "after 1: sb(A)");
    let inputs = unwalked_inputs(&case, A).await;
    assert_eq!(inputs, 0, "after 1: derivation(A).unwalked_inputs");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 1, 0, 0, 0], "after 1: counters(E1)");
    let output = case.output_of(B, "out").await;
    let stored = case.cached_path(B_OUT).await.id;
    let link = (output.is_cached, output.cached_path);
    assert_eq!(link, (true, Some(stored)), "after 1: output(B,out)");
    let asked = case.ids(&[A]).await;
    assert_eq!(as_set(&report.to_probe), asked, "after 1: report.to_probe");
    case.zero_drift("step 1").await;

    case.upstream_probed(&[A]).await;

    let a = case.sb(A).await;
    assert_eq!((a.probed, a.status), (true, Created), "after 2: sb(A)");
    case.zero_drift("step 2").await;

    case.commit_nar(drv_nar(A)).await;

    let status = case.sb(A).await.status;
    assert_eq!(status, Queued, "after 3: sb(A).status");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 1, 0, 1, 0], "after 3: counters(E1)");
    case.zero_drift("step 3").await;
    case.finish().await;
}

#[tokio::test]
async fn b10_a_derivation_without_a_stored_output_is_not_substituted() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;
    let batch = a_above_a_walked_b(&case, e1).await;

    case.walk(batch).await;

    let skipped = SharedBuild {
        status: Skipped,
        ..Default::default()
    };

    assert_eq!(shared_build(&case, B).await, skipped, "after 1: sb(B)");
    let output = case.output_of(B, "out").await;
    assert!(!output.is_cached, "after 1: output(B,out).is_cached");
    let blocking_deps = case.sb(A).await.blocking_deps;
    assert_eq!(blocking_deps, 1, "after 1: sb(A).blocking_deps");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 1, 0, 0, 0], "after 1: counters(E1)");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn b11_an_existing_shared_build_with_a_stored_output_keeps_its_status() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = Case::start(chain_done(e1).evaluation(e2)).await;
    let attempt = case.sb(B).await.attempt;
    let b = case.tasked(e2, vec![walked(B, B_OUT, &[])]).await;

    case.walk(b).await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.attempt),
        (Completed, attempt),
        "after 1: sb(B)"
    );
    assert!(named(&case, e2, B).await, "after 1: build_job(E2,B)");
    let counters = case.counters(e2).await;
    assert_eq!(counters, [1, 0, 0, 0, 0], "after 1: counters(E2)");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn b12_an_entry_point_is_not_duplicated_and_a_batch_without_a_task_creates_none() {
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
            .build_status(Queued),
    )
    .await;

    let a = with_attr(walked(A, A_OUT, &[]), "pkg.a");
    let c = with_attr(walked(C, C_OUT, &[]), "pkg.c");

    walk(&case, e1, vec![a]).await;

    let entry_points = case.entry_points(e1).await.len();
    assert_eq!(entry_points, 1, "after 1: entry points of E1");
    let status = case.sb(A).await.status;
    assert_eq!(status, Queued, "after 1: sb(A).status");
    case.zero_drift("step 1").await;

    case.walk(Batch {
        derivations: vec![c],
        ..batch(e1)
    })
    .await;

    let entry_point = case.entry_point(e1, "pkg.c").await;
    assert_eq!(entry_point, None, "after 2: entry_point(E1, pkg.c)");
    let derivation = case.derivation(C).await;
    assert!(derivation.walked, "after 2: derivation(C).walked");
    assert!(named(&case, e1, C).await, "after 2: build_job(E1,C)");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 1, 0, 1, 0], "after 2: counters(E1)");
    let c = case.sb(C).await;
    assert_eq!((c.status, c.wanted), (Skipped, false), "after 2: sb(C)");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn b13_ifd_marks_the_derivation_and_a_plain_derivation_stays_unmarked() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;
    let a = with_ifd(with_attr(walked(A, A_OUT, &[]), "pkg.a"));
    let b = with_attr(walked(B, B_OUT, &[]), "pkg.b");

    walk(&case, e1, vec![a, b]).await;

    let (a, b) = (case.derivation(A).await, case.derivation(B).await);
    assert_eq!((a.ifd, b.ifd), (true, false), "after 1: ifd of A, B");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn b14_walking_a_referenced_derivation_gives_its_referrer_a_runtime_edge() {
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
            .depends_on(A)
            .build_status(Queued)
            .derivation(A)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(A_OUT)
            .build_status(Completed)
            .cached_output(A)
            .references(&[(A_OUT, "a"), (B_OUT, "b")]),
    )
    .await;

    let gv = case.gv(e1).await;

    let report = walk(&case, e1, vec![walked(B, B_OUT, &[])]).await;

    let kind = case.edge_kind(A, B).await;
    assert_eq!(kind, Some(EdgeKind::Runtime), "after 1: edge(A,B).kind");
    let a = case.sb(A).await;
    let columns = (a.status, a.missing_runtime_deps, a.fetchable, a.wanted);
    assert_eq!(columns, (Completed, 1, false, true), "after 1: sb(A)");
    let x = case.sb(X).await;
    let columns = (x.status, x.blocking_deps, x.wanted);
    assert_eq!(columns, (Created, 1, true), "after 1: sb(X)");
    let b = case.sb(B).await;
    let columns = (b.status, b.wanted, b.probed, b.blocking_deps);
    assert_eq!(columns, (Created, true, false, 0), "after 1: sb(B)");
    assert!(named(&case, e1, B).await, "after 1: build_job(E1,B)");
    let asked = case.ids(&[B]).await;
    assert_eq!(as_set(&report.to_probe), asked, "after 1: report.to_probe");
    assert_eq!(case.gv(e1).await, gv + 2, "after 1: gv(E1)");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [3, 2, 0, 0, 0], "after 1: counters(E1)");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn b15_a_batch_writes_no_cache_available_on_a_second_producer_of_a_known_output() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        shape()
            .evaluation(e1)
            .derivation(F1)
            .walked()
            .entry_point("pkg.f1")
            .job()
            .probed()
            .output(F_OUT)
            .cache_available()
            .build_status(Queued),
    )
    .await;

    let f2 = with_attr(walked(F2, F_OUT, &[]), "pkg.f2");

    let report = walk(&case, e1, vec![f2]).await;

    let f2 = case.sb(F2).await;
    let columns = (f2.status, f2.cache_available, f2.probed, f2.wanted);
    assert_eq!(columns, (Created, false, false, true), "after 1: sb(F2)");
    let output = case.output_of(F2, "out").await;
    assert_eq!(output.external_url, None, "after 1: output(F2,out)");
    let f1 = case.sb(F1).await;
    let columns = (f1.status, f1.cache_available);
    assert_eq!(columns, (Queued, true), "after 1: sb(F1)");
    let asked = case.ids(&[F2]).await;
    assert_eq!(as_set(&report.to_probe), asked, "after 1: report.to_probe");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 2, 0, 1, 0], "after 1: counters(E1)");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn b16_an_empty_batch_touches_only_graph_version() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(chain(e1)).await;
    let (rows, gv) = (case.rows().await, case.gv(e1).await);

    let report = walk(&case, e1, Vec::new()).await;

    assert_eq!(report.walked, 0, "after 1: report.walked");
    assert_eq!(report.to_probe, vec![], "after 1: report.to_probe");
    assert_eq!(case.rows().await, rows, "after 1: all rows of CHAIN");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 2, 0, 1, 0], "after 1: counters(E1)");
    assert_eq!(case.gv(e1).await, gv + 1, "after 1: gv(E1)");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn b17_derivations_walked_in_the_same_batch_are_not_stubbed_and_all_go_to_the_probe() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;
    let b = walked(B, B_OUT, &[C]);
    let c = walked(C, C_OUT, &[]);

    let report = walk(&case, e1, vec![a_above_b(), b, c]).await;

    for path in [A, B, C] {
        let row = case.derivation(path).await;
        let columns = (row.walked, row.unwalked_inputs);
        assert_eq!(columns, (true, 0), "after 1: derivation({path})");
    }

    assert_eq!(report.walked, 3, "after 1: report.walked");
    let asked = case.ids(&[A, B, C]).await;
    assert_eq!(as_set(&report.to_probe), asked, "after 1: report.to_probe");
    let known = case.known(&[A, B, C]).await;
    assert_eq!(known, [A, B, C], "after 1: known_derivations");
    let a = case.sb(A).await;
    assert_eq!((a.status, a.wanted), (Created, true), "after 1: sb(A)");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [3, 1, 0, 0, 0], "after 1: counters(E1)");
    for path in [B, C] {
        let status = case.sb(path).await.status;
        assert_eq!(status, Skipped, "after 1: sb({path}).status");
    }

    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn b18_a_second_evaluation_walking_a_stored_subtree_names_all_of_it_and_walks_nothing() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = Case::start(chain(e1).evaluation(e2)).await;
    let gv = (case.gv(e1).await, case.gv(e2).await);

    let report = walk(&case, e2, vec![a_above_b()]).await;

    assert_eq!(report.walked, 0, "after 1: report.walked");
    let jobs = [named(&case, e2, A).await, named(&case, e2, B).await];
    assert_eq!(jobs, [true, true], "after 1: jobs of E2 on A, B");
    let entry_point = case.entry_point(e2, "pkg.a").await;
    assert!(entry_point.is_some(), "after 1: entry_point(E2, pkg.a)");
    let after = (case.gv(e1).await, case.gv(e2).await);
    assert_eq!(after, (gv.0, gv.1 + 1), "after 1: gv of E1, E2");
    let statuses = (case.sb(B).await.status, case.sb(A).await.status);
    assert_eq!(statuses, (Queued, Created), "after 1: sb(B), sb(A)");
    let counters = [case.counters(e2).await, case.counters(e1).await];
    let expected = [[2, 2, 0, 1, 0]; 2];
    assert_eq!(counters, expected, "after 1: counters of E2, E1");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn b19_a_kind_upgrade_to_both_is_a_grown_edge_set_for_the_other_naming_evaluation() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
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
            .build_status(Queued)
            .derivation(B)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .build_status(Queued)
            .evaluation(e2)
            .derivation(A)
            .job()
            .derivation(B)
            .job(),
    )
    .await;

    let gv = (case.gv(e1).await, case.gv(e2).await);

    walk(&case, e2, vec![a_above_b()]).await;

    let kind = case.edge_kind(A, B).await;
    assert_eq!(kind, Some(EdgeKind::Both), "after 1: edge(A,B).kind");
    let inputs = unwalked_inputs(&case, A).await;
    assert_eq!(inputs, 0, "after 1: derivation(A).unwalked_inputs");
    let after = (case.gv(e1).await, case.gv(e2).await);
    assert_eq!(after, (gv.0 + 2, gv.1 + 3), "after 1: gv of E1, E2");
    let a = case.sb(A).await;
    let columns = (a.blocking_deps, a.missing_runtime_deps, a.status);
    assert_eq!(columns, (1, 1, Created), "after 1: sb(A)");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn b20_a_batch_with_errors_stores_the_message_and_the_verdict_is_failed() {
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
            .build_status(Completed)
            .cached_output(A),
    )
    .await;

    let message = "attribute pkg.b failed to evaluate";

    case.walk(Batch {
        errors: vec![message.to_owned()],
        ..case.tasked(e1, Vec::new()).await
    })
    .await;

    let errors = case.errors(e1).await;
    assert_eq!(errors, [message], "after 1: error messages of E1");
    let status = case.evaluation(e1).await.status;
    let evaluating = EvaluationStatus::EvaluatingDerivation;
    assert_eq!(status, evaluating, "after 1: evaluation(E1).status");
    case.zero_drift("step 1").await;
    let completed = Transition::EvalStreamCompleted { evaluation: e1 };

    case.transition(completed).await;

    let status = case.evaluation(e1).await.status;
    assert_eq!(status, EvaluationStatus::Failed, "after 2: status of E1");
    case.zero_drift("step 2").await;
    case.finish().await;
}
