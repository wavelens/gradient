/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeSet;

use chrono::Duration;
use gradient_db::maintenance::gc::scan_deletable_path_hashes;
use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::AttemptOutcome;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::EvaluationId;
use gradient_fixtures::shape::{Shape, shape};
use gradient_wire::types::BuildFailureKind;

use super::{A, B, Case, chain_done, store_path, t0};
use crate::messages::{Demotion, GcRequest};

const B_INPUT_SOURCE_PATCH_STORE_PATH_NAME: &str = "b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5-sunrpc.patch";

fn store_path_name_hash_part(name: &str) -> &str {
    name.split_once('-').map_or(name, |(hash, _)| hash)
}

fn entry_derivation_depends_on_derivation_with_cached_input_source(
    evaluation: EvaluationId,
) -> Shape {
    chain_done(evaluation)
        .derivation(B)
        .add_input_source_and_cache_path(B_INPUT_SOURCE_PATCH_STORE_PATH_NAME)
}

async fn assert_dependent_walked_with_one_unwalked_input_and_neither_derivation_known(case: &Case) {
    let (a, b) = (case.derivation(A).await, case.derivation(B).await);
    assert_eq!(
        (b.walked, a.walked, a.unwalked_inputs),
        (false, true, 1),
        "B no longer walked after 1. A: one unwalked input."
    );
    let known = case.known(&[A, B]).await;
    assert!(
        known.is_empty(),
        "after 1: the server doesn't report A and B as known: {known:?}"
    );
}

async fn assert_server_reports_derivations_a_and_b_as_known(case: &Case) {
    let known: BTreeSet<String> = case.known(&[A, B]).await.into_iter().collect();
    let expected = BTreeSet::from([A.to_owned(), B.to_owned()]);
    assert_eq!(known, expected, "before: the evaluator can skip A and B");
}

#[tokio::test]
async fn k01_cache_expiry_scan_keeps_drv_file_and_input_source_of_unreachable_walked_derivation_then_lists_them_after_orphan_gc_deletes_it()
 {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        shape()
            .evaluation(e1)
            .derivation(B)
            .walked()
            .probed()
            .drv_cached()
            .build_status(BuildStatus::Skipped)
            .add_input_source_and_cache_path(B_INPUT_SOURCE_PATCH_STORE_PATH_NAME)
            .created_at(t0() - Duration::hours(48)),
    )
    .await;
    let recorded = BTreeSet::from([
        store_path_name_hash_part(B).to_owned(),
        store_path_name_hash_part(B_INPUT_SOURCE_PATCH_STORE_PATH_NAME).to_owned(),
    ]);
    let collect_expired_cached_store_path_hashes = || async {
        scan_deletable_path_hashes(&case.ctx.worker_db, gradient_types::now())
            .await
            .expect("the scan runs")
            .into_iter()
            .collect::<BTreeSet<String>>()
    };

    let listed = collect_expired_cached_store_path_hashes().await;

    assert!(
        listed.is_disjoint(&recorded),
        "after 1: .drv files and input sources of recorded derivations survive the scan: {listed:?}"
    );
    case.zero_drift("step 1").await;

    let candidates = case.ids(&[B]).await.into_iter().collect();
    let report = case.gc(GcRequest::Derivations { candidates }).await;

    assert_eq!(
        report.deleted_derivations.len(),
        1,
        "after 2: B no longer exists"
    );
    let listed = collect_expired_cached_store_path_hashes().await;
    assert_eq!(
        listed, recorded,
        "After 2: the scan can list `.drv` files and input sources of deleted derivations."
    );
    case.zero_drift("step 2").await;

    case.gc(GcRequest::Paths {
        hashes: listed.into_iter().collect(),
        scanned_at: t0(),
    })
    .await;

    for hash in &recorded {
        let path = case.find_cached_path(hash).await;
        assert!(path.is_none(), "after 3: the listed paths are gone: {hash}");
    }
    case.zero_drift("step 3").await;
    case.finish().await;
}

#[tokio::test]
async fn removing_cached_input_source_marks_derivation_not_walked_and_its_dependent_not_already_known()
 {
    let e1 = EvaluationId::now_v7();
    let case =
        Case::start(entry_derivation_depends_on_derivation_with_cached_input_source(e1)).await;
    assert_server_reports_derivations_a_and_b_as_known(&case).await;
    let source = store_path_name_hash_part(B_INPUT_SOURCE_PATCH_STORE_PATH_NAME);

    case.demote(Demotion::MissingNar {
        hash: source.to_owned(),
    })
    .await;

    let path = case.find_cached_path(source).await;
    assert!(
        path.is_none(),
        "After 1: the deleted path disappeared from the cache"
    );
    assert_dependent_walked_with_one_unwalked_input_and_neither_derivation_known(&case).await;
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn removing_dependent_derivation_drv_from_cache_marks_it_unwalked_and_dependent_derivation_not_known()
 {
    let e1 = EvaluationId::now_v7();
    let case =
        Case::start(entry_derivation_depends_on_derivation_with_cached_input_source(e1)).await;
    assert_server_reports_derivations_a_and_b_as_known(&case).await;
    let drv_file = store_path_name_hash_part(B);

    case.demote(Demotion::MissingNar {
        hash: drv_file.to_owned(),
    })
    .await;

    let path = case.find_cached_path(drv_file).await;
    assert!(
        path.is_none(),
        "After 1: the deleted path disappeared from the cache"
    );
    assert_dependent_walked_with_one_unwalked_input_and_neither_derivation_known(&case).await;
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn expiry_gc_paths_keeps_live_input_source_and_removes_missing_nar_file_marks_derivation_not_walked_and_stops_known_report()
 {
    let e1 = EvaluationId::now_v7();
    let case =
        Case::start(entry_derivation_depends_on_derivation_with_cached_input_source(e1)).await;
    let source = store_path_name_hash_part(B_INPUT_SOURCE_PATCH_STORE_PATH_NAME);
    case.gc(GcRequest::Paths {
        hashes: vec![source.to_owned()],
        scanned_at: t0(),
    })
    .await;
    let path = case.find_cached_path(source).await;
    assert!(
        path.is_some(),
        "before: expiry recheck can keep live input sources"
    );
    assert_server_reports_derivations_a_and_b_as_known(&case).await;

    case.gc(GcRequest::MissingNarFileCachedPathHashes {
        hashes: vec![source.to_owned()],
    })
    .await;

    let path = case.find_cached_path(source).await;
    assert!(
        path.is_none(),
        "after 1: the cached_path row with the missing NAR file is gone"
    );
    assert_dependent_walked_with_one_unwalked_input_and_neither_derivation_known(&case).await;
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn failed_build_with_missing_input_source_and_missing_storage_object_marks_derivation_as_not_walked()
 {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        chain_done(e1)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(B)
            .input_source(B_INPUT_SOURCE_PATCH_STORE_PATH_NAME)
            .derivation(A)
            .build_status(BuildStatus::Building)
            .attempt(AttemptOutcome::Running, None)
            .open_assignment(),
    )
    .await;
    assert_server_reports_derivations_a_and_b_as_known(&case).await;
    let (hash, package) = B_INPUT_SOURCE_PATCH_STORE_PATH_NAME
        .split_once('-')
        .expect("a store path name");

    case.build_failed(
        A,
        BuildFailureKind::InputsUnavailable,
        &[store_path(hash, package)],
    )
    .await;

    assert_dependent_walked_with_one_unwalked_input_and_neither_derivation_known(&case).await;
    case.zero_drift("step 1").await;
    case.finish().await;
}
