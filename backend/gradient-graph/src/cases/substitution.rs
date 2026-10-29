/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::EvaluationId;
use gradient_fixtures::shape::{Shape, shape};

use super::{
    A, A_OUT, B, B_OUT, B_OUT2, C, C_OUT, C_OUT2, Case, D, D_OUT, chain, commit_of, narinfo,
};

fn a_builds_with(evaluation: EvaluationId, outputs_of_b: &[&str]) -> Shape {
    shape()
        .evaluation(evaluation)
        .evaluation_status(EvaluationStatus::Building)
        .derivation(A)
        .walked()
        .entry_point("pkg.a")
        .job()
        .probed()
        .drv_cached()
        .output(A_OUT)
        .depends_on_outputs(B, outputs_of_b)
        .derivation(B)
        .walked()
        .job()
        .drv_cached()
        .output(B_OUT)
        .named_output(B_OUT2, "dev")
        .depends_on(C)
        .derivation(C)
        .walked()
        .job()
        .drv_cached()
        .output(C_OUT)
        .build_status(BuildStatus::Skipped)
}

async fn upstream_holds(case: &Case, outputs: &[(&str, &str)]) {
    let hits: Vec<_> = outputs
        .iter()
        .map(|(hash, package)| (*hash, narinfo(&[(hash, package)])))
        .collect();
    case.upstream_hits(&hits).await;
    case.upstream_probed(&[B]).await;
}

async fn substitute_b(case: &Case, evaluation: EvaluationId, outputs: &[(&str, &str)]) {
    case.assigned(evaluation, B, true)
        .await
        .expect("a worker claims the passthrough B");
    case.build_started(B).await;
    for (hash, package) in outputs {
        case.commit_nar(commit_of(hash, package, &[(hash, package)]))
            .await;
    }
    case.build_output(B, true).await;
    case.build_completed(B).await;
    case.close_assignment(B).await;
}

#[tokio::test]
async fn s01_a_build_needing_only_out_fetches_it_and_leaves_the_inputs_of_its_producer_unbuilt() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(a_builds_with(e1, &["out"])).await;

    upstream_holds(&case, &[(B_OUT, "b")]).await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.cache_available, b.unneeded_outputs),
        (BuildStatus::Queued, true, vec!["dev".to_owned()]),
        "after 1: sb(B) status, cache_available, unneeded_outputs"
    );
    let c = case.sb(C).await;
    assert_eq!(
        (c.status, c.wanted),
        (BuildStatus::Skipped, false),
        "after 1: sb(C) status, wanted"
    );
    case.zero_drift("step 1").await;

    substitute_b(&case, e1, &[(B_OUT, "b")]).await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.fetchable),
        (BuildStatus::Substituted, true),
        "after 2: sb(B) status, fetchable without its dev output"
    );
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.blocking_deps),
        (BuildStatus::Queued, 0),
        "after 2: sb(A) status, blocking_deps"
    );
    let c = case.sb(C).await.status;
    assert_eq!(c, BuildStatus::Skipped, "after 2: sb(C).status");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn s02_a_second_build_needing_the_missing_output_turns_its_producer_into_a_real_build() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        a_builds_with(e1, &["out"])
            .derivation(D)
            .walked()
            .entry_point("pkg.d")
            .job()
            .drv_cached()
            .output(D_OUT)
            .depends_on_outputs(B, &["dev"]),
    )
    .await;
    upstream_holds(&case, &[(B_OUT, "b")]).await;
    substitute_b(&case, e1, &[(B_OUT, "b")]).await;

    case.upstream_probed(&[D]).await;

    let d = case.sb(D).await;
    assert_eq!(
        (d.status, d.wanted, d.blocking_deps),
        (BuildStatus::Created, true, 1),
        "after 1: sb(D) waits for the dev output no cache holds"
    );
    case.zero_drift("step 1").await;

    let turned = case.requeue_needed_outputs().await;

    assert_eq!(turned, 1, "after 2: shared builds turned into real builds");
    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.cache_available, b.unneeded_outputs, b.fetchable),
        (BuildStatus::Created, false, Vec::<String>::new(), false),
        "after 2: sb(B) status, cache_available, unneeded_outputs, fetchable"
    );
    let c = case.sb(C).await;
    assert_eq!(
        (c.status, c.wanted),
        (BuildStatus::Created, true),
        "after 2: sb(C) status, wanted"
    );
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.blocking_deps),
        (BuildStatus::Created, 1),
        "after 2: sb(A) status, blocking_deps"
    );
    let d = case.sb(D).await.blocking_deps;
    assert_eq!(d, 1, "after 2: sb(D).blocking_deps");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn s03_a_producer_with_every_output_upstream_is_fetched_whole() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(a_builds_with(e1, &["out"])).await;

    upstream_holds(&case, &[(B_OUT, "b"), (B_OUT2, "b-dev")]).await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.cache_available, b.unneeded_outputs),
        (BuildStatus::Queued, true, Vec::<String>::new()),
        "after 1: sb(B) status, cache_available, unneeded_outputs"
    );
    case.zero_drift("step 1").await;

    substitute_b(&case, e1, &[(B_OUT, "b"), (B_OUT2, "b-dev")]).await;

    let (a, b, c) = (case.sb(A).await, case.sb(B).await, case.sb(C).await);
    assert_eq!(
        (b.status, b.fetchable),
        (BuildStatus::Substituted, true),
        "after 2: sb(B) status, fetchable"
    );
    assert_eq!(a.status, BuildStatus::Queued, "after 2: sb(A).status");
    assert_eq!(c.status, BuildStatus::Skipped, "after 2: sb(C).status");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn s04_a_producer_no_cache_holds_is_built() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(a_builds_with(e1, &["out"])).await;

    case.upstream_probed(&[B]).await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.cache_available, b.unneeded_outputs),
        (BuildStatus::Created, false, Vec::<String>::new()),
        "after 1: sb(B) status, cache_available, unneeded_outputs"
    );
    let c = case.sb(C).await;
    assert_eq!(
        (c.status, c.wanted),
        (BuildStatus::Created, true),
        "after 1: sb(C) status, wanted"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn s05_a_build_needing_an_output_no_cache_holds_keeps_its_producer_a_real_build() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(a_builds_with(e1, &["out", "dev"])).await;

    upstream_holds(&case, &[(B_OUT, "b")]).await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.cache_available, b.unneeded_outputs),
        (BuildStatus::Created, false, Vec::<String>::new()),
        "after 1: sb(B) status, cache_available, unneeded_outputs"
    );
    let c = case.sb(C).await;
    assert_eq!(
        (c.status, c.wanted),
        (BuildStatus::Created, true),
        "after 1: sb(C) status, wanted"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn s06_an_output_no_cache_holds_does_not_need_the_other_outputs_of_a_referenced_producer() {
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
            .depends_on_outputs(B, &["out"])
            .derivation(B)
            .walked()
            .job()
            .drv_cached()
            .output(B_OUT)
            .named_output(B_OUT2, "dev")
            .depends_on_outputs(C, &["out"])
            .derivation(C)
            .walked()
            .job()
            .drv_cached()
            .output(C_OUT)
            .named_output(C_OUT2, "man")
            .build_status(BuildStatus::Skipped),
    )
    .await;

    case.upstream_hits(&[
        (B_OUT, narinfo(&[(B_OUT, "b"), (C_OUT, "c")])),
        (C_OUT, narinfo(&[(C_OUT, "c")])),
    ])
    .await;
    case.upstream_probed(&[B, C]).await;

    let (b, c) = (case.sb(B).await, case.sb(C).await);
    assert_eq!(
        (b.cache_available, b.unneeded_outputs),
        (true, vec!["dev".to_owned()]),
        "after 1: sb(B) cache_available, unneeded_outputs"
    );
    assert_eq!(
        (c.cache_available, c.unneeded_outputs),
        (true, vec!["man".to_owned()]),
        "after 1: sb(C) cache_available, unneeded_outputs"
    );
    case.zero_drift("step 1").await;

    let turned = case.requeue_needed_outputs().await;

    assert_eq!(turned, 0, "after 2: shared builds turned into real builds");
    let c = case.sb(C).await;
    assert_eq!(
        (c.cache_available, c.unneeded_outputs),
        (true, vec!["man".to_owned()]),
        "after 2: sb(C) cache_available, unneeded_outputs"
    );
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn s07_a_producer_whose_needed_output_our_cache_already_holds_is_not_built() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(a_builds_with(e1, &["out"]).cached_output(B)).await;

    case.upstream_probed(&[B]).await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.cache_available, b.unneeded_outputs),
        (true, vec!["dev".to_owned()]),
        "after 1: sb(B) cache_available, unneeded_outputs"
    );
    let c = case.sb(C).await;
    assert_eq!(
        (c.status, c.wanted),
        (BuildStatus::Skipped, false),
        "after 1: sb(C) status, wanted"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn s08_a_producer_probed_before_our_cache_held_its_needed_output_is_not_built() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        a_builds_with(e1, &["out"])
            .cached_output(B)
            .derivation(B)
            .probed()
            .derivation(C)
            .build_status(BuildStatus::Created),
    )
    .await;

    case.upstream_probed(&[B]).await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.cache_available, b.unneeded_outputs),
        (true, vec!["dev".to_owned()]),
        "after 1: sb(B) cache_available, unneeded_outputs"
    );
    assert!(
        !case.sb(C).await.wanted,
        "after 1: sb(C) is no longer wanted"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn s09_a_parent_turning_available_frees_the_partly_held_input_it_blocked() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        a_builds_with(e1, &["out"])
            .cached_output(B)
            .derivation(B)
            .probed()
            .derivation(C)
            .build_status(BuildStatus::Created)
            .derivation(D)
            .walked()
            .entry_point("pkg.d")
            .job()
            .probed()
            .drv_cached()
            .output(D_OUT)
            .depends_on_outputs(B, &["dev"]),
    )
    .await;

    case.upstream_hits(&[(D_OUT, narinfo(&[(D_OUT, "d")]))])
        .await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.cache_available, b.unneeded_outputs),
        (true, vec!["dev".to_owned()]),
        "after 1: sb(B) cache_available, unneeded_outputs"
    );
    assert!(
        case.sb(D).await.cache_available,
        "after 1: sb(D).cache_available"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn s10_a_miss_holds_only_while_its_build_is_wanted() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(chain(e1)).await;

    case.upstream_hits(&[(A_OUT, narinfo(&[(A_OUT, "a")]))])
        .await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.wanted, b.probed),
        (BuildStatus::Skipped, false, false),
        "after 1: sb(B) status, wanted, probed"
    );
    case.zero_drift("step 1").await;

    case.upstream_probed(&[B]).await;

    assert!(!case.sb(B).await.probed, "after 2: sb(B).probed");
    case.zero_drift("step 2").await;
    case.finish().await;
}
