/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;
use std::time::Duration;

use gradient_db::store_path_content_variant_trust::ProvenWorkerCredentialIds;
use gradient_entity::RotatedHashMapping;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::{CacheId, EvaluationId};
use gradient_entity::project_cache::{self, CacheSubscriptionMode};
use gradient_entity::store_path_content_producer::{
    self, CacheUploadChannel, StoredContentVariantProducerKind,
};
use gradient_entity::worker_registration;
use gradient_entity::{cached_path, cached_path_signature};
use gradient_fixtures::shape::{Shape, shape};
use gradient_util::shutdown::Shutdown;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, TransactionTrait};

use super::{
    A, A_OUT, A2, A2_OUT, B, B_OUT, C, C_OUT, Case, D, D_OUT, F, F_OUT, G, G_OUT, H, H_OUT, I,
    I_OUT, M, M_OUT, N, N_OUT, R_OUT, SOURCE, X, X_OUT, Y_OUT, Z, Z_OUT, batch, chain, commit_of,
    drv, nar, with_attr, with_output, with_verified_content_from_nar_bytes,
};
use crate::messages::{Demotion, GcRequest, NarCommit, NarCommitContent, StoredContentProducer};
use crate::test_ctx::{Batch, BatchReport};
use gradient_types::DerivationId;

fn commit_of_b(producer: StoredContentProducer) -> NarCommit {
    NarCommit {
        references: vec![format!("{B_OUT}-b")],
        producer,
        ..nar(B_OUT, "b")
    }
}

fn content_upload_producer_for_binary_cache_over_rest(cache: CacheId) -> StoredContentProducer {
    StoredContentProducer::Upload {
        caches: vec![cache],
        user: None,
        api_key: None,
        channel: CacheUploadChannel::RestHttpApiUpload,
    }
}

async fn walk_above_b(
    case: &Case,
    evaluation: EvaluationId,
    path: &str,
    output: &str,
    attr: &str,
) -> BatchReport {
    case.walk(Batch {
        derivations: vec![
            crate::messages::EvaluationBatchDerivation::test_batch_entry_from_discovered_derivation(
                with_attr(with_output(drv(path, &[B]), output), attr),
            ),
        ],
        task: Some(case.db.task_of(evaluation).await),
        ..batch(evaluation)
    })
    .await
}

async fn row(case: &Case, hash: &str, cache: CacheId, line: &str) -> cached_path_signature::Model {
    case.signature(hash, cache)
        .await
        .unwrap_or_else(|| panic!("{line}: no signature row of {hash}"))
}

async fn signed(case: &Case, hash: &str, cache: CacheId, line: &str) -> Vec<u8> {
    row(case, hash, cache, line)
        .await
        .signature
        .unwrap_or_else(|| panic!("{line}: the signature of {hash} is null"))
}

async fn signed_over(
    case: &Case,
    hash: &str,
    cache: CacheId,
    content: &NarCommit,
    line: &str,
) -> Vec<u8> {
    let row = row(case, hash, cache, line).await;
    assert!(case.verifies(&row, content).await, "{line}: {row:?}");
    signed(case, hash, cache, line).await
}

async fn unchanged(case: &Case, hash: &str, cache: CacheId, earlier: &[u8], line: &str) {
    assert_eq!(signed(case, hash, cache, line).await, earlier, "{line}");
}

async fn null_row(case: &Case, hash: &str, cache: CacheId, line: &str) {
    assert_eq!(row(case, hash, cache, line).await.signature, None, "{line}");
}

async fn no_row(case: &Case, hash: &str, cache: CacheId, line: &str) {
    assert_eq!(case.signature(hash, cache).await, None, "{line}");
}

#[tokio::test]
async fn k01_a_keyed_project_cache_gets_a_signed_row_and_an_unkeyed_cache_a_null_row() {
    let (e1, c1, c2) = (EvaluationId::now_v7(), CacheId::now_v7(), CacheId::now_v7());
    let case = Case::start(chain(e1).into_cache(c1).keyed(Case::key()).into_cache(c2)).await;
    let commit = commit_of_b(case.make_worker_producer_for_evaluation_project(e1).await);

    case.commit_nar(commit.clone()).await;

    signed_over(&case, B_OUT, c1, &commit, "after 1: C1 verifies with K1").await;
    null_row(&case, B_OUT, c2, "after 1: C2 null").await;
    let rows = case.signatures(B_OUT).await.len();
    assert_eq!(rows, 2, "after 1: 2 signature rows of B_OUT");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn k02_a_single_cache_target_writes_a_single_row_and_target_none_writes_none() {
    let (e1, c1, c2) = (EvaluationId::now_v7(), CacheId::now_v7(), CacheId::now_v7());
    let case = Case::start(
        chain(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .into_cache(c2)
            .keyed(Case::key())
            .without_cache_signing(),
    )
    .await;

    case.commit_nar(commit_of_b(
        content_upload_producer_for_binary_cache_over_rest(c2),
    ))
    .await;

    null_row(&case, B_OUT, c2, "after 1: C2 null, a private path").await;
    no_row(&case, B_OUT, c1, "after 1: no row in C1").await;
    case.zero_drift("step 1").await;

    case.commit_nar(NarCommit {
        producer: StoredContentProducer::Upload {
            caches: Vec::new(),
            user: None,
            api_key: None,
            channel: CacheUploadChannel::RestHttpApiUpload,
        },
        ..nar(SOURCE, "source")
    })
    .await;

    let rows = case.signatures(SOURCE).await;
    assert!(rows.is_empty(), "after 2: no row of the source: {rows:?}");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn k03_unsigned_cache_subscription_worker_output_commits_unsigned_row_even_when_another_project_signs_that_cache()
 {
    for signing_producer in [false, true] {
        let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
        let c1 = CacheId::now_v7();
        let private = chain(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .without_cache_signing();

        let case = Case::start(if signing_producer {
            private.evaluation(e2).into_cache(c1)
        } else {
            private
        })
        .await;

        let commit = commit_of_b(case.make_worker_producer_for_evaluation_project(e1).await);
        case.commit_nar(commit).await;

        null_row(
            &case,
            B_OUT,
            c1,
            "After 1: C1 is null for both producer shapes",
        )
        .await;
        case.zero_drift("step 1").await;

        run_publish_round_and_assert_success(&case, 10).await;

        null_row(
            &case,
            B_OUT,
            c1,
            "After publish pass: C1 is still null. Projects that sign C1 have no job that needs B.",
        )
        .await;
        case.zero_drift("step 2").await;
        case.finish().await;
    }
}

#[tokio::test]
async fn k03b_unsigned_worker_output_commit_with_signing_subscription_signs_copy_with_cache_row_and_cache_serves_same_signed_copy()
 {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let c1 = CacheId::now_v7();
    let case = Case::start(
        chain(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .without_cache_signing()
            .evaluation(e2)
            .into_cache(c1)
            .derivation(B)
            .job(),
    )
    .await;

    let commit = commit_of_b(case.make_worker_producer_for_evaluation_project(e1).await);
    let committed = case.commit_nar(commit.clone()).await;

    signed_over(
        &case,
        B_OUT,
        c1,
        &commit,
        "after 1: commit signature with the C1 key on the copy with the C1 row",
    )
    .await;
    let served = gradient_db::store_path_content_variant_reader_choice::get_content_variants_served_by_binary_cache_for_store_path_hashes(
        &case.db.connection(),
        c1,
        &[B_OUT.to_owned()],
    )
    .await
    .expect("Tests read the variants that the binary cache served.");
    assert_eq!(
        served.get(B_OUT).map(|row| row.id),
        Some(committed.cached_path),
        "After 1: C1 can provide a copy that it signed"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn k04_a_repeated_worker_commit_with_the_same_content_changes_no_signature() {
    let (e1, c1, c2) = (EvaluationId::now_v7(), CacheId::now_v7(), CacheId::now_v7());
    let case = Case::start(
        chain(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .into_cache(c2)
            .keyed(Case::key()),
    )
    .await;

    let commit = |cache| {
        with_verified_content_from_nar_bytes(
            commit_of_b(content_upload_producer_for_binary_cache_over_rest(cache)),
            b"fh",
        )
    };

    let report = case.commit_nar(commit(c1)).await;

    assert_eq!(report.signed, vec![c1], "after 1: report.signed");
    let targeted = signed(&case, B_OUT, c1, "after 1: C1 non-null").await;
    let claimed = signed(&case, B_OUT, c2, "after 1: C2 signed by the claim").await;
    case.zero_drift("step 1").await;

    let report = case.commit_nar(commit(c1)).await;

    assert!(!report.created, "after 2: report.created == false");
    assert_eq!(report.signed, vec![c1], "after 2: report.signed");
    unchanged(&case, B_OUT, c1, &targeted, "after 2: C1 as after 1").await;
    let rows = case.signatures(B_OUT).await.len();
    assert_eq!(rows, 2, "after 2: the target row and the claimed row");
    case.zero_drift("step 2").await;

    let report = case.commit_nar(commit(c2)).await;

    assert_eq!(report.signed, vec![c2], "after 3: only the new target");
    unchanged(&case, B_OUT, c2, &claimed, "after 3: C2 unchanged").await;
    unchanged(&case, B_OUT, c1, &targeted, "after 3: C1 unchanged").await;
    case.zero_drift("step 3").await;
    case.finish().await;
}

#[tokio::test]
async fn k05_different_project_commits_new_content_for_already_uploaded_store_path_and_each_cache_signs_only_its_own_content()
 {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let (c1, c2) = (CacheId::now_v7(), CacheId::now_v7());
    let case = Case::start(
        chain(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .evaluation(e2)
            .into_cache(c2)
            .keyed(Case::key())
            .worker_registration("w2"),
    )
    .await;

    let uploaded = with_verified_content_from_nar_bytes(
        commit_of_b(content_upload_producer_for_binary_cache_over_rest(c1)),
        b"fh1",
    );
    case.commit_nar(uploaded.clone()).await;

    let in_c1 = signed_over(
        &case,
        B_OUT,
        c1,
        &uploaded,
        "After 1: signature of uploaded content by C1",
    )
    .await;
    no_row(
        &case,
        B_OUT,
        c2,
        "No row in C2 after 1. Project B doesn't read C1.",
    )
    .await;
    case.zero_drift("step 1").await;

    let built = with_verified_content_from_nar_bytes(
        commit_of_b(case.make_worker_producer_for_evaluation_project(e2).await),
        b"fh2",
    );
    let report = case.commit_nar(built.clone()).await;

    let variants = case
        .db
        .stored_contents_for_store_path_hash_oldest_first(B_OUT)
        .await;
    assert_eq!(
        variants.len(),
        2,
        "After 2: second stored contents of B_OUT"
    );
    assert!(report.created, "After 2: second stored contents of B_OUT");
    assert_eq!(
        report.signed,
        vec![c2],
        "Use C2 to sign the contents of B's worker after step 2."
    );
    signed_over(
        &case,
        B_OUT,
        c2,
        &built,
        "Use C2 to sign the contents of B's worker after step 2.",
    )
    .await;
    let in_c2 = row(
        &case,
        B_OUT,
        c2,
        "Use C2 to sign the contents of B's worker after step 2.",
    )
    .await;
    assert_eq!(
        in_c2.cached_path, variants[1].id,
        "Use C2 to sign the contents of B's worker after step 2."
    );
    unchanged(&case, B_OUT, c1, &in_c1, "After 2: C1 unchanged").await;
    let first_content_caches: Vec<CacheId> = case
        .signatures(B_OUT)
        .await
        .into_iter()
        .filter(|signature| signature.cached_path == variants[0].id)
        .map(|signature| signature.cache)
        .collect();
    assert_eq!(
        first_content_caches,
        vec![c1],
        "After 2: the first content kept only its signature in C1"
    );
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn k06_a_batch_claims_the_cached_outputs_of_its_jobs_for_keyed_signing_projects() {
    let (e1, e2, e3) = (
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
    );
    let (c0, c1, c2, c3) = (
        CacheId::now_v7(),
        CacheId::now_v7(),
        CacheId::now_v7(),
        CacheId::now_v7(),
    );
    let case = Case::start(
        shape()
            .evaluation(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .into_cache(c0)
            .evaluation(e2)
            .into_cache(c2)
            .keyed(Case::key())
            .without_cache_signing()
            .evaluation(e3)
            .into_cache(c3)
            .keyed(Case::key())
            .derivation(B)
            .walked()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .build_status(BuildStatus::Completed)
            .cached_output(B),
    )
    .await;

    for evaluation in [e1, e2] {
        case.add_worker_registration_producer_for_evaluation_project_to_stored_path(
            B_OUT, evaluation,
        )
        .await;
    }
    let sig_old = vec![7; 64];
    case.seed_signature(B_OUT, c3, Some(sig_old.clone())).await;

    walk_above_b(&case, e1, A, A_OUT, "pkg.a").await;

    signed(&case, B_OUT, c1, "after 1: C1 non-null").await;
    no_row(&case, B_OUT, c0, "after 1: no row in C0, no key").await;
    case.zero_drift("step 1").await;

    walk_above_b(&case, e2, A, A_OUT, "pkg.a").await;

    no_row(&case, B_OUT, c2, "after 2: no row in C2").await;
    case.zero_drift("step 2").await;

    walk_above_b(&case, e3, A, A_OUT, "pkg.a").await;

    unchanged(&case, B_OUT, c3, &sig_old, "after 3: C3 == SIG_OLD").await;
    case.zero_drift("step 3").await;
    case.finish().await;
}

#[tokio::test]
async fn k11_a_claimed_output_brings_the_cached_paths_it_references_into_the_cache() {
    let e1 = EvaluationId::now_v7();
    let c1 = CacheId::now_v7();
    let case = Case::start(
        shape()
            .evaluation(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .derivation(B)
            .walked()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .build_status(BuildStatus::Completed)
            .cached_path(X_OUT, "x")
            .cached_output(B)
            .references(&[(B_OUT, "b"), (X_OUT, "x")]),
    )
    .await;

    walk_above_b(&case, e1, A, A_OUT, "pkg.a").await;

    signed(&case, B_OUT, c1, "after 1: C1 holds B").await;
    signed(&case, X_OUT, c1, "after 1: C1 holds what B references").await;
    case.zero_drift("step 1").await;
    case.finish().await;
}

fn b_above_x_above_y(e1: EvaluationId, c1: CacheId) -> Shape {
    shape()
        .evaluation(e1)
        .into_cache(c1)
        .keyed(Case::key())
        .derivation(B)
        .walked()
        .probed()
        .drv_cached()
        .output(B_OUT)
        .build_status(BuildStatus::Completed)
        .cached_path(Y_OUT, "y")
        .cached_path(X_OUT, "x")
        .references(&[(Y_OUT, "y")])
        .cached_output(B)
        .references(&[(B_OUT, "b"), (X_OUT, "x")])
}

async fn cached_signature_deletion_count_matches_cache(
    case: &Case,
    hash: &str,
    cache: CacheId,
    line: &str,
) -> bool {
    row(case, hash, cache, line)
        .await
        .reference_closure_established_at_cache_deletion_count
        == Some(case.cache_deleted_path_signature_count(cache).await)
}

#[tokio::test]
async fn k12_a_claim_walk_stops_at_a_cache_row_whose_references_are_already_claimed_and_leaves_paths_below_without_rows()
 {
    let (e1, c1) = (EvaluationId::now_v7(), CacheId::now_v7());
    let case = Case::start(b_above_x_above_y(e1, c1)).await;
    let earlier = vec![7; 64];
    case.seed_signature_with_references_claimed(X_OUT, c1, Some(earlier.clone()))
        .await;

    walk_above_b(&case, e1, A, A_OUT, "pkg.a").await;

    signed(&case, B_OUT, c1, "after 1: C1 holds B").await;
    unchanged(&case, X_OUT, c1, &earlier, "after 1: X keeps its signature").await;
    no_row(&case, Y_OUT, c1, "after 1: the claim stopped at X").await;
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn k14_claim_walk_on_signed_claim_row_with_unclaimed_references_keeps_signature_bytes_and_sets_deletion_count()
 {
    let (e1, c1) = (EvaluationId::now_v7(), CacheId::now_v7());
    let case = Case::start(b_above_x_above_y(e1, c1)).await;
    let earlier = vec![7; 64];
    case.seed_signature(X_OUT, c1, Some(earlier.clone())).await;

    walk_above_b(&case, e1, A, A_OUT, "pkg.a").await;

    signed(&case, B_OUT, c1, "after 1: C1 holds B").await;
    unchanged(&case, X_OUT, c1, &earlier, "after 1: X keeps its signature").await;
    let marked =
        cached_signature_deletion_count_matches_cache(&case, X_OUT, c1, "after 1: X").await;
    assert!(marked, "after 1: X has its references claimed");
    signed(&case, Y_OUT, c1, "after 1: C1 holds what X references").await;
    case.zero_drift("step 1").await;
    case.finish().await;
}

async fn start_test_case_with_claimed_b_above_x_above_y_in_c1(
    e1: EvaluationId,
    c1: CacheId,
) -> Case {
    let case = Case::start(b_above_x_above_y(e1, c1)).await;
    for hash in [B_OUT, X_OUT, Y_OUT] {
        case.seed_signature_with_references_claimed(hash, c1, None)
            .await;
    }
    case
}

async fn assert_c1_b_and_y_are_not_established_and_c1_deletion_count_is_one_after_removing_x_claim(
    case: &Case,
    c1: CacheId,
) {
    assert!(
        !cached_signature_deletion_count_matches_cache(case, B_OUT, c1, "after 1: B").await,
        "after 1: B in C1 can no longer stop the walk, since C1 lost its claim row"
    );
    assert!(
        !cached_signature_deletion_count_matches_cache(case, Y_OUT, c1, "after 1: Y").await,
        "after 1: Y in C1 can no longer stop the walk either, but Y doesn't reference X"
    );
    assert_eq!(
        case.cache_deleted_path_signature_count(c1).await,
        1,
        "after 1: removal of a claim for X -> one deletion in C1"
    );
}

#[tokio::test]
async fn demoting_cache_claim_increments_deletion_count_so_earlier_rows_do_not_stop_walk() {
    let (e1, c1) = (EvaluationId::now_v7(), CacheId::now_v7());
    let case = start_test_case_with_claimed_b_above_x_above_y_in_c1(e1, c1).await;

    case.demote(Demotion::CacheClaim {
        cache: c1,
        hash: X_OUT.into(),
    })
    .await;

    assert_c1_b_and_y_are_not_established_and_c1_deletion_count_is_one_after_removing_x_claim(
        &case, c1,
    )
    .await;
    case.finish().await;
}

#[tokio::test]
async fn retiring_cached_path_cascades_to_signatures_and_increments_cache_deletion_count_so_earlier_rows_do_not_stop_walk()
 {
    let (e1, c1) = (EvaluationId::now_v7(), CacheId::now_v7());
    let case = start_test_case_with_claimed_b_above_x_above_y_in_c1(e1, c1).await;

    case.gc(GcRequest::RetireCachedPathsWithMissingNarFiles {
        ids: vec![case.cached_path(X_OUT).await.id],
    })
    .await;

    assert_eq!(
        case.find_cached_path(X_OUT).await,
        None,
        "after 1: X retired"
    );
    assert_c1_b_and_y_are_not_established_and_c1_deletion_count_is_one_after_removing_x_claim(
        &case, c1,
    )
    .await;
    case.finish().await;
}

#[tokio::test]
async fn k15_walked_path_references_are_claimed_only_after_all_references_are_stored() {
    let (e1, c1) = (EvaluationId::now_v7(), CacheId::now_v7());
    let case = Case::start(
        shape()
            .evaluation(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .derivation(B)
            .walked()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .build_status(BuildStatus::Completed)
            .cached_path(X_OUT, "x")
            .cached_output(B)
            .references(&[(B_OUT, "b"), (X_OUT, "x"), (Z_OUT, "z")]),
    )
    .await;

    walk_above_b(&case, e1, A, A_OUT, "pkg.a").await;

    let in_c1 = signed(&case, B_OUT, c1, "after 1: C1 holds B").await;
    signed(&case, X_OUT, c1, "after 1: C1 holds X").await;
    let x_marked =
        cached_signature_deletion_count_matches_cache(&case, X_OUT, c1, "after 1: X").await;
    assert!(x_marked, "after 1: X references nothing unstored");
    let b_marked =
        cached_signature_deletion_count_matches_cache(&case, B_OUT, c1, "after 1: B").await;
    assert!(!b_marked, "after 1: B references the unstored Z");
    case.zero_drift("step 1").await;

    case.commit_nar(nar(Z_OUT, "z")).await;
    crate::claims::claim_committed_paths(&case.ctx, &[B_OUT.to_owned()])
        .await
        .expect("the claim is written");

    signed(&case, Z_OUT, c1, "after 2: C1 holds Z").await;
    unchanged(&case, B_OUT, c1, &in_c1, "after 2: B keeps its signature").await;
    let b_marked =
        cached_signature_deletion_count_matches_cache(&case, B_OUT, c1, "after 2: B").await;
    assert!(b_marked, "after 2: every reference of B is stored");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn k13_a_committed_job_output_with_uploading_project_signing_targets_signs_itself_and_a_referenced_path_with_no_derivation_into_the_project_keyed_cache()
 {
    let (e1, c1) = (EvaluationId::now_v7(), CacheId::now_v7());
    let case = Case::start(
        chain(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .cached_path(X_OUT, "x"),
    )
    .await;

    case.commit_nar(NarCommit {
        references: vec![format!("{B_OUT}-b"), format!("{X_OUT}-x")],
        ..commit_of_b(case.make_worker_producer_for_evaluation_project(e1).await)
    })
    .await;

    signed(&case, B_OUT, c1, "after 1: C1 holds B").await;
    signed(&case, X_OUT, c1, "after 1: C1 holds what B references").await;
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn k07_unread_content_is_not_signed_into_either_waiting_project_cache_by_committed_paths_claim_and_shared_build_is_not_marked_done()
 {
    let (e1, e2, e3) = (
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
    );
    let (c1, c2, c3) = (CacheId::now_v7(), CacheId::now_v7(), CacheId::now_v7());
    let case = Case::start(
        chain(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .evaluation(e2)
            .into_cache(c2)
            .keyed(Case::key())
            .derivation(B)
            .job()
            .evaluation(e3)
            .into_cache(c3)
            .keyed(Case::key()),
    )
    .await;
    let before = case.sb(B).await.status;

    case.commit_nar(commit_of_b(
        content_upload_producer_for_binary_cache_over_rest(c3),
    ))
    .await;

    no_row(&case, B_OUT, c1, "C1 has no row after 1").await;
    no_row(&case, B_OUT, c2, "No matching line in C2 after 1.").await;
    case.zero_drift("step 1").await;

    crate::claims::claim_committed_paths(&case.ctx, &[B_OUT.to_owned()])
        .await
        .expect("the claim is written");

    no_row(&case, B_OUT, c1, "After 2: no C1 row after claim").await;
    no_row(&case, B_OUT, c2, "After 2: no C2 line after the assertion").await;
    let status = case.sb(B).await.status;
    assert_eq!(status, before, "sb(B).status unchanged after 2 runs");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn k08_a_null_row_is_filled_by_a_later_claim_from_a_signing_project() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let c1 = CacheId::now_v7();
    let case = Case::start(
        chain(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .without_cache_signing()
            .evaluation(e2)
            .into_cache(c1)
            .derivation(B)
            .build_status(BuildStatus::Completed)
            .cached_output(B)
            .derivation(A)
            .build_status(BuildStatus::Queued),
    )
    .await;

    case.seed_signature(B_OUT, c1, None).await;

    walk_above_b(&case, e2, A2, A2_OUT, "pkg.a2").await;

    signed(&case, B_OUT, c1, "after 1: C1 non-null").await;
    let rows = case.signatures(B_OUT).await.len();
    assert_eq!(rows, 1, "after 1: a single signature row of B_OUT");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn k09_a_drv_path_without_a_producer_is_signed_for_the_uploading_project() {
    let (e1, c1) = (EvaluationId::now_v7(), CacheId::now_v7());
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
            .output(B_OUT)
            .into_cache(c1)
            .keyed(Case::key()),
    )
    .await;

    let (drv_hash, package) = B.split_once('-').expect("B is a drv path");

    case.commit_nar(NarCommit {
        producer: case.make_worker_producer_for_evaluation_project(e1).await,
        ..nar(drv_hash, package)
    })
    .await;

    signed(&case, drv_hash, c1, "after 1: B.drv in C1 non-null").await;
    let status = case.sb(B).await.status;
    assert_eq!(status, BuildStatus::Queued, "after 1: sb(B).status");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn k10_a_signature_verifies_over_the_outbound_paths() {
    let (e1, c1) = (EvaluationId::now_v7(), CacheId::now_v7());
    let case = Case::start_with_mapping(
        chain(e1).into_cache(c1).keyed(Case::key()),
        Arc::new(RotatedHashMapping),
    )
    .await;
    let stored = commit_of_b(case.make_worker_producer_for_evaluation_project(e1).await);
    case.commit_nar(stored.clone()).await;

    let mapper = case.ctx.cache_mapper(c1);
    let seen = NarCommit {
        store_path: mapper
            .outbound_text(&stored.store_path)
            .expect("the stored path maps"),
        references: stored
            .references
            .iter()
            .map(|reference| mapper.outbound_text(reference).expect("a reference maps"))
            .collect(),
        ..stored.clone()
    };
    let row = row(&case, B_OUT, c1, "k10").await;
    assert!(case.verifies(&row, &seen).await, "{row:?}");
    assert!(!case.verifies(&row, &stored).await, "{row:?}");
    case.finish().await;
}

#[tokio::test]
async fn k13_output_in_other_project_cache_does_not_make_shared_build_substituted_or_signed_but_same_project_derivation_is_substituted()
 {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let (c1, c2) = (CacheId::now_v7(), CacheId::now_v7());
    let case = Case::start(
        shape()
            .evaluation(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .evaluation(e2)
            .into_cache(c2)
            .keyed(Case::key()),
    )
    .await;
    for (hash, package) in [(A_OUT, "a"), (B_OUT, "b")] {
        case.commit_nar(NarCommit {
            producer: content_upload_producer_for_binary_cache_over_rest(c1),
            ..nar(hash, package)
        })
        .await;
    }

    case.walk(Batch {
        derivations: vec![
            crate::messages::EvaluationBatchDerivation::test_batch_entry_from_discovered_derivation(
                with_attr(with_output(drv(A, &[]), A_OUT), "pkg.a"),
            ),
        ],
        task: Some(case.db.task_of(e1).await),
        ..batch(e1)
    })
    .await;

    let status = case.sb(A).await.status;
    assert_eq!(
        status,
        BuildStatus::Substituted,
        "The label after 1 is `sb(A).status`. Project A can read C1."
    );

    case.walk(Batch {
        derivations: vec![
            crate::messages::EvaluationBatchDerivation::test_batch_entry_from_discovered_derivation(
                with_attr(with_output(drv(B, &[]), B_OUT), "pkg.b"),
            ),
        ],
        task: Some(case.db.task_of(e2).await),
        ..batch(e2)
    })
    .await;

    let status = case.sb(B).await.status;
    assert_ne!(
        status,
        BuildStatus::Substituted,
        "After 2: sb(B).status. Project B doesn't read C1."
    );
    no_row(&case, B_OUT, c2, "After 2: no row in C2").await;
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn k14_read_only_cache_subscription_gets_no_signature_row_from_worker_commit_committed_path_claim_or_signature_backfill()
 {
    let (e1, c1) = (EvaluationId::now_v7(), CacheId::now_v7());
    let case = Case::start(
        chain(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .worker_registration("w1"),
    )
    .await;
    project_cache::Entity::update_many()
        .col_expr(
            project_cache::Column::Mode,
            Expr::value(CacheSubscriptionMode::ReadOnly),
        )
        .col_expr(project_cache::Column::SignOutputs, Expr::value(false))
        .filter(project_cache::Column::Cache.eq(c1))
        .exec(&case.db.connection())
        .await
        .expect("the subscription becomes read-only");

    case.commit_nar(commit_of_b(
        case.make_worker_producer_for_evaluation_project(e1).await,
    ))
    .await;

    no_row(
        &case,
        B_OUT,
        c1,
        "After 1: no row in read-only C1 after the worker commit",
    )
    .await;

    crate::claims::claim_committed_paths(&case.ctx, &[B_OUT.to_owned()])
        .await
        .expect("the claim is written");

    no_row(&case, B_OUT, c1, "After 2: no line after assertion").await;

    gradient_db::store_path_content_variant_trust::insert_unsigned_signature_placeholders_for_trusted_store_path_content_variants(
        &case.db.connection(),
        case.db.project_of(e1).await,
        c1,
    )
    .await
    .expect("the backfill runs");

    no_row(&case, B_OUT, c1, "After 3: no rows after backfill").await;
    case.zero_drift("step 3").await;
    case.finish().await;
}

#[tokio::test]
async fn k19_worker_commit_signs_only_signing_enabled_cache_subscription_for_read_write_and_write_only_modes_and_leaves_signing_disabled_subscription_unsigned_with_outside_binary_cache_reader_not_found()
 {
    for cache_subscription_mode_without_sign_outputs in [
        CacheSubscriptionMode::ReadWrite,
        CacheSubscriptionMode::WriteOnly,
    ] {
        let e1 = EvaluationId::now_v7();
        let (signing_cache_id, cache_id_without_sign_outputs) =
            (CacheId::now_v7(), CacheId::now_v7());
        let case = Case::start(
            chain(e1)
                .into_cache(signing_cache_id)
                .keyed(Case::key())
                .into_cache(cache_id_without_sign_outputs)
                .keyed(Case::key()),
        )
        .await;
        project_cache::Entity::update_many()
            .col_expr(
                project_cache::Column::Mode,
                Expr::value(cache_subscription_mode_without_sign_outputs),
            )
            .col_expr(project_cache::Column::SignOutputs, Expr::value(false))
            .filter(project_cache::Column::Cache.eq(cache_id_without_sign_outputs))
            .exec(&case.db.connection())
            .await
            .expect("A subscription can stop signing outputs.");
        let commit = commit_of_b(case.make_worker_producer_for_evaluation_project(e1).await);

        case.commit_nar(commit.clone()).await;

        signed_over(
            &case,
            B_OUT,
            signing_cache_id,
            &commit,
            "The signed cache verified B_OUT after 1.",
        )
        .await;
        null_row(
            &case,
            B_OUT,
            cache_id_without_sign_outputs,
            "After 1: Caches that don't sign keep the unsigned rows of B_OUT.",
        )
        .await;
        let cached_path_row = gradient_db::store_path_content_variant_reader_choice::get_content_variants_served_by_binary_cache_for_store_path_hashes(
            &case.db.connection(),
            cache_id_without_sign_outputs,
            &[B_OUT.to_owned()],
        )
        .await
        .expect("Tests read the variants that the binary cache served.")
        .remove(B_OUT);
        assert_eq!(
            cached_path_row, None,
            "After 1: the external reader of the unsigned cache can't find B_OUT."
        );
        case.zero_drift("step 1").await;
        case.finish().await;
    }
}

#[tokio::test]
async fn k20_project_member_ssh_store_rest_build_request_upload_writes_signed_row_only_to_signing_read_write_cache_and_unsigned_row_without_upload_record_to_read_write_cache_with_sign_outputs_off_and_outside_binary_cache_reader_finds_nothing()
 {
    let e1 = EvaluationId::now_v7();
    let (signing_cache_id, cache_id_without_sign_outputs) = (CacheId::now_v7(), CacheId::now_v7());
    let case = Case::start(
        chain(e1)
            .into_cache(signing_cache_id)
            .keyed(Case::key())
            .into_cache(cache_id_without_sign_outputs)
            .keyed(Case::key()),
    )
    .await;
    project_cache::Entity::update_many()
        .col_expr(project_cache::Column::SignOutputs, Expr::value(false))
        .filter(project_cache::Column::Cache.eq(cache_id_without_sign_outputs))
        .exec(&case.db.connection())
        .await
        .expect("A subscription can stop signing outputs.");
    let commit = commit_of_b(StoredContentProducer::ProjectUserContentUpload {
        project: case.db.project_of(e1).await,
        user: gradient_fixtures::rows::user().id,
        channel: CacheUploadChannel::SshNixStoreUpload,
    });

    case.commit_nar(commit.clone()).await;

    signed_over(
        &case,
        B_OUT,
        signing_cache_id,
        &commit,
        "The signing cache can verify B_OUT after step 1. A project member uploaded B_OUT.",
    )
    .await;
    null_row(
        &case,
        B_OUT,
        cache_id_without_sign_outputs,
        "Expect B_OUT in the cache as an unsigned line after step 1 when signing is off.",
    )
    .await;
    let upload_record_count_for_cache_with_sign_outputs_off =
        store_path_content_producer::Entity::find()
            .filter(store_path_content_producer::Column::Cache.eq(cache_id_without_sign_outputs))
            .all(&case.db.connection())
            .await
            .expect("Upload records load.")
            .len();
    assert_eq!(
        upload_record_count_for_cache_with_sign_outputs_off, 0,
        "After 1: caches with signing off contain no record that B_OUT was uploaded there."
    );
    let cached_path_row = gradient_db::store_path_content_variant_reader_choice::get_content_variants_served_by_binary_cache_for_store_path_hashes(
        &case.db.connection(),
        cache_id_without_sign_outputs,
        &[B_OUT.to_owned()],
    )
    .await
    .expect("Tests read the variants that the binary cache served.")
    .remove(B_OUT);
    assert_eq!(
        cached_path_row, None,
        "An external reader of a cache with signing off can't find B_OUT after 1."
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn test_project_member_ssh_store_rest_build_request_upload_to_write_cache_with_sign_outputs_off_stores_path_as_unsigned_row_without_upload_record()
 {
    let e1 = EvaluationId::now_v7();
    let cache_id_without_sign_outputs = CacheId::now_v7();
    let case = Case::start(
        chain(e1)
            .into_cache(cache_id_without_sign_outputs)
            .keyed(Case::key()),
    )
    .await;
    project_cache::Entity::update_many()
        .col_expr(project_cache::Column::SignOutputs, Expr::value(false))
        .filter(project_cache::Column::Cache.eq(cache_id_without_sign_outputs))
        .exec(&case.db.connection())
        .await
        .expect("A subscription can stop signing outputs.");
    let b_out_uploader_project_id = case.db.project_of(e1).await;

    case.commit_nar(commit_of_b(
        StoredContentProducer::ProjectUserContentUpload {
            project: b_out_uploader_project_id,
            user: gradient_fixtures::rows::user().id,
            channel: CacheUploadChannel::RestHttpApiUpload,
        },
    ))
    .await;

    null_row(
        &case,
        B_OUT,
        cache_id_without_sign_outputs,
        "Unsigned rows for B_OUT still remain in the project's only write cache after step 1, even with signing off.",
    )
    .await;
    let project_member_uploaded_b_out_cached_path_row_ids = cached_path::Entity::find()
        .filter(cached_path::Column::Hash.eq(B_OUT))
        .all(&case.db.connection())
        .await
        .expect("Stored copies of B_OUT load.")
        .into_iter()
        .map(|row| row.id);
    let b_out_uploaded_content_variant_upload_producer_kind_and_member_project_id_pairs =
        store_path_content_producer::Entity::find()
            .filter(
                store_path_content_producer::Column::CachedPath
                    .is_in(project_member_uploaded_b_out_cached_path_row_ids),
            )
            .all(&case.db.connection())
            .await
            .expect("Load producer rows of uploads.")
            .into_iter()
            .filter(|row| {
                matches!(
                    row.kind,
                    StoredContentVariantProducerKind::UploadedByUserOrApiKey
                        | StoredContentVariantProducerKind::UploadedByProjectMember
                )
            })
            .map(|row| (row.kind, row.uploading_member_project_id))
            .collect::<Vec<_>>();
    assert_eq!(
        b_out_uploaded_content_variant_upload_producer_kind_and_member_project_id_pairs,
        [(
            StoredContentVariantProducerKind::UploadedByProjectMember,
            Some(b_out_uploader_project_id)
        )],
        "After 1, write exactly one producer row of kind UploadedByProjectMember for an upload of B_OUT by a project member. Name the source project in that row. Write no cache upload record (kind UploadedByUserOrApiKey)."
    );
    let upload_record_count_for_cache_with_sign_outputs_off =
        store_path_content_producer::Entity::find()
            .filter(store_path_content_producer::Column::Cache.eq(cache_id_without_sign_outputs))
            .all(&case.db.connection())
            .await
            .expect("Upload records load.")
            .len();
    assert_eq!(
        upload_record_count_for_cache_with_sign_outputs_off, 0,
        "After 1: caches with signing off contain no record that B_OUT was uploaded there."
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

async fn start_ssh_output_upload_test_case_with_unsigned_cache_subscription_and_signing_cache_in_second_project()
-> (Case, EvaluationId, EvaluationId) {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let (cache_id_without_sign_outputs, c2) = (CacheId::now_v7(), CacheId::now_v7());
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
            .depends_on(B)
            .derivation(B)
            .walked()
            .job()
            .drv_cached()
            .output(B_OUT)
            .depends_on(C)
            .derivation(C)
            .walked()
            .job()
            .drv_cached()
            .output(C_OUT)
            .build_status(BuildStatus::Skipped)
            .into_cache(cache_id_without_sign_outputs)
            .keyed(Case::key())
            .evaluation(e2)
            .into_cache(c2)
            .keyed(Case::key()),
    )
    .await;
    project_cache::Entity::update_many()
        .col_expr(project_cache::Column::SignOutputs, Expr::value(false))
        .filter(project_cache::Column::Cache.eq(cache_id_without_sign_outputs))
        .exec(&case.db.connection())
        .await
        .expect("A subscription can stop signing outputs.");
    case.commit_nar(commit_of_b(
        StoredContentProducer::ProjectUserContentUpload {
            project: case.db.project_of(e1).await,
            user: gradient_fixtures::rows::user().id,
            channel: CacheUploadChannel::SshNixStoreUpload,
        },
    ))
    .await;

    (case, e1, e2)
}

#[tokio::test]
async fn pending_own_build_does_not_run_when_unsigned_member_upload_is_available_when_probed() {
    let (case, ..) =
        start_ssh_output_upload_test_case_with_unsigned_cache_subscription_and_signing_cache_in_second_project()
            .await;

    case.upstream_probed(&[B]).await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.cache_available, b.attempt),
        (true, 0),
        "B can use a stored output after the probe. A member of B's own project uploaded that output to the cache with signatures disabled."
    );
    case.finish().await;
}

#[tokio::test]
async fn other_project_with_keyed_signing_read_write_cache_does_not_trust_unsigned_copy_that_source_project_trusts()
 {
    let (case, e1, e2) =
        start_ssh_output_upload_test_case_with_unsigned_cache_subscription_and_signing_cache_in_second_project()
            .await;
    let project_trusts_copy_of_b_out = async |evaluation| {
        let project = case.db.project_of(evaluation).await;
        case.db
            .texts(&format!(
                "SELECT EXISTS (SELECT 1 FROM cached_path v WHERE v.hash = '{B_OUT}' AND {trusted})::text",
                trusted = gradient_db::store_path_content_variant_trust::project_trusts_content_variant_exists_sql(
                    "v.id",
                    &format!("'{project}'::uuid"),
                ),
            ))
            .await
            == ["true"]
    };

    assert_eq!(
        (
            project_trusts_copy_of_b_out(e1).await,
            project_trusts_copy_of_b_out(e2).await
        ),
        (true, false),
        "P does trust the stored copy of B_OUT that its members uploaded. Q is another project with its own signature cache. Q does not trust that copy."
    );
    case.finish().await;
}

#[tokio::test]
async fn k15_claimed_output_does_not_pull_referenced_store_path_uploaded_only_by_other_project_into_project_cache()
 {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let (c1, c2) = (CacheId::now_v7(), CacheId::now_v7());
    let case = Case::start(
        shape()
            .evaluation(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .evaluation(e2)
            .into_cache(c2)
            .keyed(Case::key())
            .derivation(B)
            .walked()
            .entry_point("pkg.b")
            .job()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .build_status(BuildStatus::Queued),
    )
    .await;
    case.commit_nar(NarCommit {
        producer: content_upload_producer_for_binary_cache_over_rest(c1),
        ..nar(X_OUT, "x")
    })
    .await;

    case.commit_nar(NarCommit {
        producer: case.make_worker_producer_for_evaluation_project(e2).await,
        ..commit_of(B_OUT, "b", &[(B_OUT, "b"), (X_OUT, "x")])
    })
    .await;

    signed(&case, B_OUT, c2, "After 2: C2 signed B").await;
    no_row(
        &case,
        X_OUT,
        c2,
        "After 2: C2 has no line for X. Only project A uploaded X.",
    )
    .await;

    cached_path_signature::Entity::update_many()
        .col_expr(
            cached_path_signature::Column::Signature,
            Expr::value(Option::<Vec<u8>>::None),
        )
        .filter(cached_path_signature::Column::Cache.eq(c2))
        .exec(&case.db.connection())
        .await
        .expect("an unsigned placeholder as the signature of B in C2");

    crate::claims::claim_committed_paths(&case.ctx, &[B_OUT.to_owned()])
        .await
        .expect("the claim is written");

    signed(
        &case,
        B_OUT,
        c2,
        "After step 3: claim can sign B in C2 again.",
    )
    .await;
    no_row(
        &case,
        X_OUT,
        c2,
        "After 3: C2 has no line for X. Only project A uploaded X.",
    )
    .await;
    case.zero_drift("step 3").await;
    case.finish().await;
}

#[tokio::test]
async fn k16_cached_output_repair_does_not_mark_shared_build_done_when_output_uploaded_to_other_project_cache()
 {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let (c1, c2) = (CacheId::now_v7(), CacheId::now_v7());
    let case = Case::start(
        chain(e2)
            .into_cache(c2)
            .keyed(Case::key())
            .evaluation(e1)
            .into_cache(c1)
            .keyed(Case::key()),
    )
    .await;
    let before = case.sb(B).await.status;
    case.commit_nar(commit_of_b(
        content_upload_producer_for_binary_cache_over_rest(c1),
    ))
    .await;

    gradient_db::graph::promotion::repair_cached_shared_builds_for_eval(&case.db.connection(), e2)
        .await
        .expect("the repair runs");

    let status = case.sb(B).await.status;
    assert_eq!(
        status, before,
        "After 1: repair doesn't change sb(B).status"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn k17_paused_inactive_worker_registration_commit_of_stored_content_is_still_signed_by_project_signing_cache()
 {
    let (e1, c1) = (EvaluationId::now_v7(), CacheId::now_v7());
    let case = Case::start(
        chain(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .worker_registration("w1"),
    )
    .await;
    let producer = case.make_worker_producer_for_evaluation_project(e1).await;
    worker_registration::Entity::update_many()
        .col_expr(worker_registration::Column::Active, Expr::value(false))
        .exec(&case.db.connection())
        .await
        .expect("worker registration in a paused state");

    let built = commit_of_b(producer);
    let report = case.commit_nar(built.clone()).await;

    assert_eq!(
        report.signed,
        vec![c1],
        "After 1: C1 signed the content of the paused worker."
    );
    signed_over(
        &case,
        B_OUT,
        c1,
        &built,
        "After 1: C1 signed the content of the paused worker.",
    )
    .await;
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn k18_commit_without_worker_credential_is_rejected_and_writes_no_stored_content_or_producer_row()
 {
    let (e1, c1) = (EvaluationId::now_v7(), CacheId::now_v7());
    let case = Case::start(chain(e1).into_cache(c1).keyed(Case::key())).await;
    let producers_before = case
        .db
        .rows::<store_path_content_producer::Entity>()
        .await
        .len();

    let result = case
        .commit_nar_result(commit_of_b(StoredContentProducer::Worker {
            project: case.db.project_of(e1).await,
            credentials: ProvenWorkerCredentialIds::default(),
            dispatched_job: None,
        }))
        .await;

    assert!(
        result.is_err(),
        "After 1: A worker without the project's credentials cannot commit."
    );
    assert!(
        case.db
            .stored_contents_for_store_path_hash_oldest_first(B_OUT)
            .await
            .is_empty()
            && case
                .db
                .rows::<store_path_content_producer::Entity>()
                .await
                .len()
                == producers_before,
        "After 1: no saved content for B_OUT and no producer row."
    );
    case.finish().await;
}

#[tokio::test]
async fn second_project_worker_with_different_bytes_for_same_store_hash_creates_new_row_and_does_not_attach_to_first_project_row()
 {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let (c1, c2) = (CacheId::now_v7(), CacheId::now_v7());
    let case = Case::start(
        chain(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .evaluation(e2)
            .into_cache(c2)
            .keyed(Case::key())
            .worker_registration("w2"),
    )
    .await;
    case.commit_nar(with_verified_content_from_nar_bytes(
        commit_of_b(content_upload_producer_for_binary_cache_over_rest(c1)),
        b"first",
    ))
    .await;

    let report = case
        .commit_nar(with_verified_content_from_nar_bytes(
            commit_of_b(case.make_worker_producer_for_evaluation_project(e2).await),
            b"second",
        ))
        .await;

    let variants = case
        .db
        .stored_contents_for_store_path_hash_oldest_first(B_OUT)
        .await;
    assert!(
        report.created && variants.len() == 2,
        "After 2: project B's worker got a separate line for B_OUT"
    );
    let worker_producers_on_first: Vec<_> = case
        .db
        .rows::<store_path_content_producer::Entity>()
        .await
        .into_iter()
        .filter(|producer| producer.cached_path == variants[0].id)
        .filter(|producer| producer.worker_registration.is_some())
        .collect();
    assert!(
        worker_producers_on_first.is_empty(),
        "After 2: the producer of the worker in project B does not attach to the content in project A"
    );
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn legacy_cached_path_row_is_not_matched_by_new_commit_and_new_row_is_server_verified() {
    let (e1, c1) = (EvaluationId::now_v7(), CacheId::now_v7());
    let case = Case::start(
        chain(e1)
            .cached_output(B)
            .into_cache(c1)
            .keyed(Case::key())
            .with_legacy_cached_path_before_producer_tracking(),
    )
    .await;

    let report = case
        .commit_nar(commit_of_b(
            content_upload_producer_for_binary_cache_over_rest(c1),
        ))
        .await;

    let variants = case
        .db
        .stored_contents_for_store_path_hash_oldest_first(B_OUT)
        .await;
    assert!(
        report.created
            && variants.len() == 2
            && !variants[0].server_computed_hashes_and_sizes
            && variants[1].server_computed_hashes_and_sizes
            && report.cached_path == variants[1].id,
        "After 1: the legacy row stayed unmatched. The commit added a server-verified row."
    );
    case.finish().await;
}

#[tokio::test]
async fn test_producer_only_commit_reuses_verified_variant_without_new_bytes() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let (c1, c2) = (CacheId::now_v7(), CacheId::now_v7());
    let case = Case::start(
        chain(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .evaluation(e2)
            .into_cache(c2)
            .keyed(Case::key())
            .worker_registration("w2"),
    )
    .await;
    let uploaded = case
        .commit_nar(commit_of_b(
            content_upload_producer_for_binary_cache_over_rest(c1),
        ))
        .await;

    let report = case
        .commit_nar(NarCommit {
            content: NarCommitContent::ExistingServerVerifiedCachedPathId(uploaded.cached_path),
            ..commit_of_b(case.make_worker_producer_for_evaluation_project(e2).await)
        })
        .await;

    let variants = case
        .db
        .stored_contents_for_store_path_hash_oldest_first(B_OUT)
        .await;
    assert!(
        !report.created && variants.len() == 1 && report.cached_path == uploaded.cached_path,
        "After 2: the commit reused the saved variant and added no lines."
    );
    assert_eq!(
        report.signed,
        vec![c2],
        "After 2: C2 signed the stored variant for the worker of project B."
    );
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn test_second_producer_same_nar_different_compression_reuses_first_content_variant_and_deletes_unused_object()
 {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let c1 = CacheId::now_v7();
    let case = Case::start(
        chain(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .evaluation(e2)
            .worker_registration("w2"),
    )
    .await;
    let nar_storage = &case.ctx.storage.nar_storage;
    let write_uploaded_object_to_storage_and_return_commit = |commit: NarCommit| async move {
        let NarCommitContent::ServerComputedNarAndCompressedFileHashesAndSizes(facts) =
            &commit.content
        else {
            unreachable!()
        };
        let key = facts.object_storage_key_for_store_path_hash_and_compressed_file_sha256(B_OUT);
        nar_storage
            .put(&key, facts.file_hash().as_bytes().to_vec())
            .await
            .expect("the NAR object is written");
        (commit, key)
    };

    let (first, first_producer_commit_zstd_frame_object_key_for_store_path_b) =
        write_uploaded_object_to_storage_and_return_commit(commit_of_b(
            content_upload_producer_for_binary_cache_over_rest(c1),
        ))
        .await;
    let first_producer_commit_result = case.commit_nar(first).await;
    let [content_variant_row_for_store_path_b] = case
        .db
        .stored_contents_for_store_path_hash_oldest_first(B_OUT)
        .await
        .try_into()
        .expect("After 1: one stored variant");

    let (second, second_producer_two_zstd_frame_commit_object_storage_key) =
        write_uploaded_object_to_storage_and_return_commit(NarCommit {
            content: NarCommitContent::ServerComputedNarAndCompressedFileHashesAndSizes(
                gradient_storage::compress_nar_bytes_as_two_zstd_frames_and_verify_hashes_and_sizes(
                    b"nar",
                ),
            ),
            ..commit_of_b(case.make_worker_producer_for_evaluation_project(e2).await)
        })
        .await;
    assert_ne!(
        first_producer_commit_zstd_frame_object_key_for_store_path_b,
        second_producer_two_zstd_frame_commit_object_storage_key,
        "Two compressions refer to two objects."
    );
    let second_producer_commit_result = case.commit_nar(second).await;

    let variants = case
        .db
        .stored_contents_for_store_path_hash_oldest_first(B_OUT)
        .await;
    assert_eq!(
        variants,
        vec![content_variant_row_for_store_path_b.clone()],
        "After 2: object key and compressed file hash remain unchanged in the row of the first producer."
    );
    assert!(
        !second_producer_commit_result.created
            && second_producer_commit_result.cached_path
                == first_producer_commit_result.cached_path
            && second_producer_commit_result.nar_object_storage_key
                == first_producer_commit_zstd_frame_object_key_for_store_path_b,
        "After 2: the first and second producers share the stored variant of the first producer."
    );
    assert!(
        nar_storage
            .exists(&first_producer_commit_zstd_frame_object_key_for_store_path_b)
            .await
            .unwrap()
            && !nar_storage
                .exists(&second_producer_two_zstd_frame_commit_object_storage_key)
                .await
                .unwrap(),
        "After 2: the first producer's object is still available and the unused second object is the one that was deleted."
    );
    case.finish().await;
}

fn object_storage_key_for_commit_of_test_store_path(commit: &NarCommit) -> String {
    let NarCommitContent::ServerComputedNarAndCompressedFileHashesAndSizes(facts) = &commit.content
    else {
        unreachable!()
    };
    facts.object_storage_key_for_store_path_hash_and_compressed_file_sha256(B_OUT)
}

#[tokio::test]
async fn commit_refuses_upload_that_skipped_write_after_demotion_deleted_object() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let c1 = CacheId::now_v7();
    let case = Case::start(
        chain(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .evaluation(e2)
            .worker_registration("w2"),
    )
    .await;
    let nar_storage = &case.ctx.storage.nar_storage;
    let first_content_upload_commit_for_store_path_b =
        commit_of_b(content_upload_producer_for_binary_cache_over_rest(c1));
    let first_commit_object_key = object_storage_key_for_commit_of_test_store_path(
        &first_content_upload_commit_for_store_path_b,
    );
    nar_storage
        .put(&first_commit_object_key, vec![0])
        .await
        .expect("the NAR object is written");
    case.commit_nar(first_content_upload_commit_for_store_path_b)
        .await;
    assert!(
        nar_storage.exists(&first_commit_object_key).await.unwrap(),
        "A second upload of the same object writes nothing, because the object already exists in storage."
    );

    case.demote(Demotion::CacheClaim {
        cache: c1,
        hash: B_OUT.into(),
    })
    .await;
    let commit_with_skipped_object_write =
        commit_of_b(case.make_worker_producer_for_evaluation_project(e2).await);
    let commit_with_skipped_object_write_result = case
        .graph
        .commit_nar(commit_with_skipped_object_write)
        .await;

    let content_variant_rows_of_store_path_b_after_refused_commit = case
        .db
        .stored_contents_for_store_path_hash_oldest_first(B_OUT)
        .await;
    assert!(
        commit_with_skipped_object_write_result.is_err()
            && content_variant_rows_of_store_path_b_after_refused_commit.is_empty(),
        "Commits after demotion fail. No rows point to the deleted object: {commit_with_skipped_object_write_result:?} {content_variant_rows_of_store_path_b_after_refused_commit:?}"
    );
    case.finish().await;
}

#[tokio::test]
async fn skipped_write_upload_is_refused_at_commit_when_garbage_collection_deletes_object_between_upload_and_commit_and_no_database_row_names_deleted_object()
 {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let c1 = CacheId::now_v7();
    let case = Case::start(
        chain(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .evaluation(e2)
            .worker_registration("w2"),
    )
    .await;
    let nar_storage = &case.ctx.storage.nar_storage;
    let first = commit_of_b(content_upload_producer_for_binary_cache_over_rest(c1));
    let key = object_storage_key_for_commit_of_test_store_path(&first);
    nar_storage
        .put(&key, vec![0])
        .await
        .expect("the NAR object is written");
    case.commit_nar(first).await;
    let store_path_b_cached_path_ids_with_missing_nar_files = case
        .db
        .stored_contents_for_store_path_hash_oldest_first(B_OUT)
        .await
        .into_iter()
        .map(|row| row.id)
        .collect();

    case.gc(GcRequest::RetireCachedPathsWithMissingNarFiles {
        ids: store_path_b_cached_path_ids_with_missing_nar_files,
    })
    .await;
    let skipped = commit_of_b(case.make_worker_producer_for_evaluation_project(e2).await);
    let refused = case.graph.commit_nar(skipped).await;

    let rows = case
        .db
        .stored_contents_for_store_path_hash_oldest_first(B_OUT)
        .await;
    assert!(
        refused.is_err() && rows.is_empty(),
        "A commit after garbage collection fails. No rows point to deleted objects: {refused:?} {rows:?}"
    );
    case.finish().await;
}

#[tokio::test]
async fn skipped_write_upload_is_refused_at_commit_when_writer_deletes_object_between_upload_and_commit()
 {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let c1 = CacheId::now_v7();
    let case = Case::start(
        chain(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .evaluation(e2)
            .worker_registration("w2"),
    )
    .await;
    let nar_storage = &case.ctx.storage.nar_storage;
    let build_commit_with_nar_as_two_zstd_frames = |producer: StoredContentProducer| NarCommit {
        content: NarCommitContent::ServerComputedNarAndCompressedFileHashesAndSizes(
            gradient_storage::compress_nar_bytes_as_two_zstd_frames_and_verify_hashes_and_sizes(
                b"nar",
            ),
        ),
        ..commit_of_b(producer)
    };

    let first = commit_of_b(content_upload_producer_for_binary_cache_over_rest(c1));
    let b_first_commit_object_key = object_storage_key_for_commit_of_test_store_path(&first);
    nar_storage
        .put(&b_first_commit_object_key, vec![0])
        .await
        .expect("the NAR object is written");
    case.commit_nar(first).await;
    let second_commit_of_two_frame_zstd_nar_for_store_path_b =
        build_commit_with_nar_as_two_zstd_frames(
            case.make_worker_producer_for_evaluation_project(e2).await,
        );
    let second_commit_object_key = object_storage_key_for_commit_of_test_store_path(
        &second_commit_of_two_frame_zstd_nar_for_store_path_b,
    );
    nar_storage
        .put(&second_commit_object_key, vec![0])
        .await
        .expect("the NAR object is written");
    assert!(
        nar_storage.exists(&second_commit_object_key).await.unwrap(),
        "The third upload can find the object that the second upload wrote and skip the write."
    );
    case.commit_nar(second_commit_of_two_frame_zstd_nar_for_store_path_b)
        .await;
    case.demote(Demotion::CacheClaim {
        cache: c1,
        hash: B_OUT.into(),
    })
    .await;
    let skipped = build_commit_with_nar_as_two_zstd_frames(
        case.make_worker_producer_for_evaluation_project(e2).await,
    );
    let refused = case.graph.commit_nar(skipped).await;

    let rows = case
        .db
        .stored_contents_for_store_path_hash_oldest_first(B_OUT)
        .await;
    assert!(
        refused.is_err() && rows.is_empty(),
        "No commit goes through after the deletion and no row points to the deleted object: {refused:?} {rows:?}"
    );
    case.finish().await;
}

#[tokio::test]
async fn empty_content_address_commit_matches_stored_content_variant_without_content_address() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let c1 = CacheId::now_v7();
    let case = Case::start(
        chain(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .evaluation(e2)
            .worker_registration("w2"),
    )
    .await;
    let first = case
        .commit_nar(commit_of_b(
            content_upload_producer_for_binary_cache_over_rest(c1),
        ))
        .await;

    let second = case
        .commit_nar_result(NarCommit {
            ca: Some(String::new()),
            ..commit_of_b(case.make_worker_producer_for_evaluation_project(e2).await)
        })
        .await
        .expect("After 2: commits with an empty content address are accepted");

    let variants = case
        .db
        .stored_contents_for_store_path_hash_oldest_first(B_OUT)
        .await;
    assert!(
        !second.created
            && second.cached_path == first.cached_path
            && variants.len() == 1
            && variants[0].ca.is_none(),
        "After 2: an empty content address matched a stored variant that has no content address"
    );
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn two_projects_keep_and_serve_their_own_content_variants_for_one_store_path() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let (c1, c2) = (CacheId::now_v7(), CacheId::now_v7());
    let case = Case::start(
        chain(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .evaluation(e2)
            .into_cache(c2)
            .keyed(Case::key())
            .worker_registration("w2"),
    )
    .await;
    let (first_project_id, second_project_id) =
        (case.db.project_of(e1).await, case.db.project_of(e2).await);

    let first = case
        .commit_nar(with_verified_content_from_nar_bytes(
            commit_of_b(case.make_worker_producer_for_evaluation_project(e1).await),
            b"first",
        ))
        .await;
    let second = case
        .commit_nar(with_verified_content_from_nar_bytes(
            commit_of_b(case.make_worker_producer_for_evaluation_project(e2).await),
            b"second",
        ))
        .await;

    assert!(
        first.created && second.created && first.cached_path != second.cached_path,
        "After 2: each worker has its own stored variant"
    );
    let worker_producer_rows_for_store_path_content_variants: Vec<_> = case
        .db
        .rows::<store_path_content_producer::Entity>()
        .await
        .into_iter()
        .filter(|producer| producer.worker_registration.is_some())
        .map(|producer| producer.cached_path)
        .collect();
    assert!(
        worker_producer_rows_for_store_path_content_variants.contains(&first.cached_path)
            && worker_producer_rows_for_store_path_content_variants.contains(&second.cached_path),
        "After 2: both producer rows for the workers exist: {worker_producer_rows_for_store_path_content_variants:?}"
    );

    let db = case.db.connection();
    let hashes = [B_OUT.to_owned()];
    for (project, cache, own_worker_cached_path_row_id) in [
        (first_project_id, c1, first.cached_path),
        (second_project_id, c2, second.cached_path),
    ] {
        let highest_ranked_trusted_content_variant = gradient_db::store_path_content_variant_reader_choice::highest_ranked_trusted_content_variant_by_store_path_hash_for_project(&db, project, &hashes)
            .await
            .expect("Read the variant that the project trusts.");
        assert_eq!(
            highest_ranked_trusted_content_variant
                .get(B_OUT)
                .map(|row| row.id),
            Some(own_worker_cached_path_row_id),
            "After 2: the project does trust its own variant"
        );
        let served_content_variants_for_store_path_hash = gradient_db::store_path_content_variant_reader_choice::get_content_variants_served_by_binary_cache_for_store_path_hashes(&db, cache, &hashes)
            .await
            .expect("Tests read the variants that the binary cache served.");
        assert_eq!(
            served_content_variants_for_store_path_hash
                .get(B_OUT)
                .map(|row| row.id),
            Some(own_worker_cached_path_row_id),
            "After 2: project caches serve their own variants"
        );
    }
    case.zero_drift("step 2").await;
    case.finish().await;
}

pub(super) fn add_waiting_build_that_depends_on_completed_build(
    seed: Shape,
    (waiting, waiting_output): (&str, &str),
    (completed, completed_output): (&str, &str),
    waiting_status: BuildStatus,
) -> Shape {
    seed.derivation(waiting)
        .walked()
        .probed()
        .drv_cached()
        .entry_point(waiting)
        .job()
        .output(waiting_output)
        .depends_on(completed)
        .build_status(waiting_status)
        .derivation(completed)
        .walked()
        .probed()
        .drv_cached()
        .job()
        .output(completed_output)
        .build_status(BuildStatus::Completed)
}

#[tokio::test]
async fn two_signing_passes_with_page_size_two_skip_queued_build_with_input_that_cannot_be_signed_and_sign_a_then_c_and_g()
 {
    let (stored_build_outputs_owner_evaluation_id, e1) =
        (EvaluationId::now_v7(), EvaluationId::now_v7());
    let c1 = CacheId::now_v7();
    let seed = shape()
        .evaluation(stored_build_outputs_owner_evaluation_id)
        .cached_path(Z_OUT, "z")
        .references(&[(Y_OUT, "y")])
        .cached_path(B_OUT, "b")
        .cached_path(D_OUT, "d")
        .cached_path(F_OUT, "f")
        .evaluation(e1)
        .evaluation_status(EvaluationStatus::Building)
        .into_cache(c1)
        .keyed(Case::key());
    let seed = [
        ((X, X_OUT), (Z, Z_OUT)),
        ((A, A_OUT), (B, B_OUT)),
        ((C, C_OUT), (D, D_OUT)),
        ((G, G_OUT), (F, F_OUT)),
    ]
    .into_iter()
    .fold(seed, |seed, (waiting, completed)| {
        add_waiting_build_that_depends_on_completed_build(
            seed,
            waiting,
            completed,
            BuildStatus::Queued,
        )
    });
    let case = Case::start(seed).await;
    let mut waiting_derivations = Vec::new();
    for path in [X, A, C, G] {
        waiting_derivations.push(case.sb(path).await.derivation);
    }
    assert!(
        waiting_derivations.is_sorted(),
        "Waiting build X, whose input cannot be signed because its stored copy refers to a path that is neither stored nor signed, sorts before the other waiting builds."
    );
    let main_cache_has_signed_copy_for_each_hash = async |hashes: [&str; 3]| {
        let mut signed = Vec::new();
        for hash in hashes {
            signed.push(
                case.signatures(hash)
                    .await
                    .iter()
                    .any(|row| row.cache == c1 && row.signature.is_some()),
            );
        }
        signed
    };

    let first_signing_pass_report = publish_one_parent_build_page(&case, None, 2).await;

    assert_eq!(
        main_cache_has_signed_copy_for_each_hash([B_OUT, D_OUT, F_OUT]).await,
        vec![true, false, false],
        "after 1: the first pass with limit 2 processed the build that can't be signed and build A. That pass signed only build A's input."
    );
    assert_eq!(
        first_signing_pass_report.last_handled_parent_build_id,
        Some(waiting_derivations[1]),
        "after 1: the first pass can report build A as the last pending build that this pass processed"
    );
    case.zero_drift("step 1").await;

    publish_one_parent_build_page(
        &case,
        first_signing_pass_report.last_handled_parent_build_id,
        2,
    )
    .await;

    assert_eq!(
        main_cache_has_signed_copy_for_each_hash([B_OUT, D_OUT, F_OUT]).await,
        vec![true, true, true],
        "After 2: the second pass can resume after build A and sign the inputs for builds C and G."
    );
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn signing_first_pass_with_page_size_two_skips_three_waiting_builds_without_signing_cache_and_signs_queued_build_g_input_into_read_write_binary_cache()
 {
    let (
        stored_build_outputs_owner_evaluation_id,
        evaluation_id_for_project_without_signing_cache,
        e1,
    ) = (
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
    );
    let c1 = CacheId::now_v7();
    let seed = shape()
        .evaluation(stored_build_outputs_owner_evaluation_id)
        .cached_path(Z_OUT, "z")
        .cached_path(B_OUT, "b")
        .cached_path(D_OUT, "d")
        .cached_path(F_OUT, "f")
        .evaluation(evaluation_id_for_project_without_signing_cache)
        .evaluation_status(EvaluationStatus::Building);
    let seed = [
        ((X, X_OUT), (Z, Z_OUT)),
        ((A, A_OUT), (B, B_OUT)),
        ((C, C_OUT), (D, D_OUT)),
    ]
    .into_iter()
    .fold(seed, |seed, (waiting, completed)| {
        add_waiting_build_that_depends_on_completed_build(
            seed,
            waiting,
            completed,
            BuildStatus::Queued,
        )
    })
    .evaluation(e1)
    .evaluation_status(EvaluationStatus::Building)
    .into_cache(c1)
    .keyed(Case::key());
    let seed = add_waiting_build_that_depends_on_completed_build(
        seed,
        (G, G_OUT),
        (F, F_OUT),
        BuildStatus::Queued,
    );
    let case = Case::start(seed).await;
    let mut waiting_derivations = Vec::new();
    for path in [X, A, C, G] {
        waiting_derivations.push(case.sb(path).await.derivation);
    }
    assert!(
        waiting_derivations.is_sorted(),
        "The three waiting builds that are not blocked sort before the waiting build blocked by missing trust."
    );

    publish_one_parent_build_page(&case, None, 2).await;

    assert!(
        case.signatures(F_OUT)
            .await
            .iter()
            .any(|row| row.cache == c1 && row.signature.is_some()),
        "after 1: The first pass with limit 2 skipped the three waiting builds that were not blocked. It signed the inputs of build G into the cache of that project."
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn periodic_signing_pass_skips_waiting_build_select_when_no_project_cache_subscription_can_sign()
 {
    let (stored_build_outputs_owner_evaluation_id, e1) =
        (EvaluationId::now_v7(), EvaluationId::now_v7());
    let seed = shape()
        .evaluation(stored_build_outputs_owner_evaluation_id)
        .cached_path(F_OUT, "f")
        .evaluation(e1)
        .evaluation_status(EvaluationStatus::Building);
    let seed = add_waiting_build_that_depends_on_completed_build(
        seed,
        (G, G_OUT),
        (F, F_OUT),
        BuildStatus::Queued,
    );
    let case = Case::start(seed).await;

    assert!(
        run_signing_transition_with_second_session_exclusive_derivation_dependency_lock(&case)
            .await
            .is_ok(),
        "No subscription is available to sign. The signing path can exit without reading pending dependency edges that another session locked and did not release."
    );
    case.finish().await;
}

#[tokio::test]
async fn periodic_signing_pass_returns_early_for_write_only_subscription_with_sign_outputs_on_and_private_key_while_derivation_dependency_is_locked()
 {
    let (stored_build_outputs_owner_evaluation_id, e1) =
        (EvaluationId::now_v7(), EvaluationId::now_v7());
    let signing_cache_id = CacheId::now_v7();
    let seed = shape()
        .evaluation(stored_build_outputs_owner_evaluation_id)
        .cached_path(F_OUT, "f")
        .evaluation(e1)
        .evaluation_status(EvaluationStatus::Building)
        .into_cache(signing_cache_id)
        .keyed(Case::key());
    let seed = add_waiting_build_that_depends_on_completed_build(
        seed,
        (G, G_OUT),
        (F, F_OUT),
        BuildStatus::Queued,
    );
    let case = Case::start(seed).await;
    project_cache::Entity::update_many()
        .col_expr(
            project_cache::Column::Mode,
            Expr::value(CacheSubscriptionMode::WriteOnly),
        )
        .col_expr(project_cache::Column::SignOutputs, Expr::value(true))
        .filter(project_cache::Column::Cache.eq(signing_cache_id))
        .exec(&case.db.connection())
        .await
        .expect("the subscription becomes write-only");

    assert!(
        run_signing_transition_with_second_session_exclusive_derivation_dependency_lock(&case)
            .await
            .is_ok(),
        "Only one signing subscription is present, with WriteOnly mode. The periodic signing pass cannot sign through that subscription. The signing pass can exit without reading pending dependency edges that another session locked and did not release."
    );
    case.finish().await;
}

#[tokio::test]
async fn publish_pass_writes_no_null_or_empty_signature_row_for_unsigned_dependency_when_project_cache_signing_key_cannot_be_used()
 {
    let e1 = EvaluationId::now_v7();
    let cache_id_with_undecryptable_signing_key = CacheId::now_v7();
    let seed = shape()
        .evaluation(EvaluationId::now_v7())
        .cached_path(F_OUT, "f")
        .evaluation(e1)
        .evaluation_status(EvaluationStatus::Building)
        .into_cache(cache_id_with_undecryptable_signing_key)
        .keyed("This is not a usable secret signing key.");
    let seed = add_waiting_build_that_depends_on_completed_build(
        seed,
        (G, G_OUT),
        (F, F_OUT),
        BuildStatus::Queued,
    );
    let case = Case::start(seed).await;
    run_publish_round_and_assert_success(&case, 64).await;

    no_row(
        &case,
        F_OUT,
        cache_id_with_undecryptable_signing_key,
        "After 1: the publish pass can't use the cache key. The pass doesn't write the signature line for F_OUT to that cache. The value is neither NULL nor empty.",
    )
    .await;
    case.finish().await;
}

async fn start_test_case_with_unsigned_dependency_and_signing_cache(e1: EvaluationId) -> Case {
    let seed = shape()
        .evaluation(EvaluationId::now_v7())
        .cached_path(F_OUT, "f")
        .evaluation(e1)
        .evaluation_status(EvaluationStatus::Building)
        .into_cache(CacheId::now_v7())
        .keyed(Case::key());
    let seed = add_waiting_build_that_depends_on_completed_build(
        seed,
        (G, G_OUT),
        (F, F_OUT),
        BuildStatus::Queued,
    );
    Case::start(seed).await
}

async fn set_sign_outputs_on_all_project_cache_rows(case: &Case, sign_outputs: bool) {
    project_cache::Entity::update_many()
        .col_expr(
            project_cache::Column::SignOutputs,
            Expr::value(sign_outputs),
        )
        .exec(&case.db.connection())
        .await
        .expect(if sign_outputs {
            "Subscriptions sign the outputs again."
        } else {
            "Subscription stopped signing outputs"
        });
}

#[tokio::test]
async fn output_signing_and_consistency_check_keep_queued_build_without_blocking_dependency_queued_with_zero_blocking_count()
 {
    let case =
        start_test_case_with_unsigned_dependency_and_signing_cache(EvaluationId::now_v7()).await;
    set_sign_outputs_on_all_project_cache_rows(&case, false).await;
    case.graph
        .run_consistency_checks()
        .await
        .expect("Consistency checks pass through the graph writer.");
    assert_eq!(
        case.queue_state(G).await,
        (BuildStatus::Queued, 0),
        "G has status Queued and has no blocking input at a time when output signing is off."
    );

    set_sign_outputs_on_all_project_cache_rows(&case, true).await;
    case.graph
        .run_consistency_checks()
        .await
        .expect("Consistency checks pass through the graph writer.");

    assert_eq!(
        case.queue_state(G).await,
        (BuildStatus::Queued, 0),
        "G still Queued with no blocking input after output signing started and a consistency check ran. Signing never required to run a build."
    );
    case.finish().await;
}

#[tokio::test]
async fn queued_build_with_two_projects_trusting_different_input_copies_has_no_blocking_input() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let seed = shape()
        .evaluation(e1)
        .evaluation_status(EvaluationStatus::Building)
        .into_cache(CacheId::now_v7())
        .keyed(Case::key())
        .cached_path(F_OUT, "f")
        .mark_uploaded_by_user();
    let seed = add_waiting_build_that_depends_on_completed_build(
        seed,
        (G, G_OUT),
        (F, F_OUT),
        BuildStatus::Queued,
    )
    .evaluation(e2)
    .evaluation_status(EvaluationStatus::Building)
    .into_cache(CacheId::now_v7())
    .keyed(Case::key())
    .worker_registration("w2")
    .derivation(G)
    .entry_point(G)
    .job()
    .derivation(F)
    .job();
    let case = Case::start(seed).await;
    let second = case
        .commit_nar(with_verified_content_from_nar_bytes(
            NarCommit {
                producer: case.make_worker_producer_for_evaluation_project(e2).await,
                ..nar(F_OUT, "f")
            },
            b"second",
        ))
        .await;
    assert!(
        second.created,
        "The second project stores its own copy of F."
    );
    case.graph
        .run_consistency_checks()
        .await
        .expect("Consistency checks pass through the graph writer.");

    assert_eq!(
        case.queue_state(G).await,
        (BuildStatus::Queued, 0),
        "G has status Queued and no blocking inputs. The two projects each trust only their own copy of F."
    );
    case.finish().await;
}

async fn run_signing_transition_with_second_session_exclusive_derivation_dependency_lock(
    case: &Case,
) -> anyhow::Result<()> {
    let second_session_derivation_dependency_exclusive_lock_transaction = case
        .db
        .connection()
        .begin()
        .await
        .expect("a second session opens a transaction");
    second_session_derivation_dependency_exclusive_lock_transaction
        .execute_unprepared("LOCK TABLE derivation_dependency IN ACCESS EXCLUSIVE MODE")
        .await
        .expect("the second session locks the dependency edges");

    let signing_pass_outcome_with_two_second_budget_while_derivation_lock_held =
        tokio::time::timeout(Duration::from_secs(2), run_publish_round(case, None, 2))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|round| round.failure.map_or(Ok(()), Err));
    second_session_derivation_dependency_exclusive_lock_transaction
        .rollback()
        .await
        .expect("the second session releases its lock");
    signing_pass_outcome_with_two_second_budget_while_derivation_lock_held
}

async fn start_case_with_five_parent_builds_where_signature_insert_for_parent_build_c_dependency_output_fails()
-> (Case, CacheId) {
    let (case, c1) = start_case_with_queued_parent_builds_and_signing_cache(5).await;
    case.sql(&format!(
        "CREATE FUNCTION reject_signature_of_dependency_output_of_parent_build_c() \
         RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
           IF NEW.signature IS NOT NULL AND EXISTS (SELECT 1 FROM cached_path \
              WHERE id = NEW.cached_path AND hash = '{D_OUT}') THEN \
             RAISE EXCEPTION 'injected'; \
           END IF; \
           RETURN NEW; \
         END $$; \
         CREATE TRIGGER reject_signature_of_dependency_output_of_parent_build_c \
           BEFORE INSERT OR UPDATE ON cached_path_signature FOR EACH ROW \
           EXECUTE FUNCTION reject_signature_of_dependency_output_of_parent_build_c();"
    ))
    .await;
    (case, c1)
}

async fn allow_signature_inserts_for_dependency_output_of_parent_build_c_again(case: &Case) {
    case.sql(
        "DROP TRIGGER reject_signature_of_dependency_output_of_parent_build_c \
         ON cached_path_signature",
    )
    .await;
}

fn end_set_aside_period_for_all_set_aside_parent_builds(case: &Case) {
    for set_aside_until in case
        .graph
        .publish_failure_page_size_and_waiting_parent_builds
        .lock()
        .set_aside_end_by_parent_build
        .values_mut()
    {
        *set_aside_until = std::time::Instant::now();
    }
}

#[tokio::test]
async fn test_failing_page_is_halved_and_single_failing_parent_build_c_is_set_aside_before_signing_others()
 {
    let (case, c1) =
        start_case_with_five_parent_builds_where_signature_insert_for_parent_build_c_dependency_output_fails().await;
    let parent_build_c: Vec<DerivationId> = case.ids(&[C]).await.into_iter().collect();

    let round = run_publish_round(&case, None, 2).await;

    assert_eq!(
        (
            round.failure.is_none(),
            round.failed_page_count,
            round.parent_build_ids_set_aside_in_publish_round,
            list_signed_dependency_outputs_in_cache(&case, c1).await,
        ),
        (true, 3, parent_build_c, vec![B_OUT, F_OUT, I_OUT, N_OUT]),
        "Page [A, C] failed. Retry with page size 1 signed A. Pages [C, G] and [C] failed. C stayed set aside temporarily. The same round produced signatures for G, H, M."
    );
    case.finish().await;
}

#[tokio::test]
async fn failing_parent_build_c_does_not_block_other_parent_builds_in_round() {
    const MAXIMUM_PARENT_BUILDS_PER_PAGE: usize = 4;
    let failed_pages_bound = 2 * MAXIMUM_PARENT_BUILDS_PER_PAGE.ilog2() as usize + 1;
    let (case, c1) =
        start_case_with_five_parent_builds_where_signature_insert_for_parent_build_c_dependency_output_fails().await;

    let round = run_publish_round(&case, None, MAXIMUM_PARENT_BUILDS_PER_PAGE).await;

    assert_eq!(
        (
            round.failure.is_none(),
            round.failed_page_count <= failed_pages_bound,
            round.parent_build_ids_set_aside_in_publish_round.len(),
            list_signed_dependency_outputs_in_cache(&case, c1).await,
        ),
        (true, true, 1, vec![B_OUT, F_OUT, I_OUT, N_OUT]),
        "A round signs every other parent build if parent build C fails. It has at most 2 * log2(maximum page size) + 1 failed pages, which is {failed_pages_bound} here. The actual count was {}.",
        round.failed_page_count
    );
    case.finish().await;
}

#[tokio::test]
async fn test_set_aside_parent_build_c_is_skipped_until_set_aside_period_ends_and_then_tried_again()
{
    let (case, c1) =
        start_case_with_five_parent_builds_where_signature_insert_for_parent_build_c_dependency_output_fails().await;
    run_publish_round(&case, None, 2).await;
    allow_signature_inserts_for_dependency_output_of_parent_build_c_again(&case).await;

    let round_while_set_aside = run_publish_round(&case, None, 2).await;

    assert_eq!(
        (
            round_while_set_aside.failure.is_none(),
            round_while_set_aside.failed_page_count,
            list_signed_dependency_outputs_in_cache(&case, c1)
                .await
                .contains(&D_OUT),
        ),
        (true, 0, false),
        "Parent build C was in its set-aside period. The next round skipped C, so D_OUT stayed unsigned even though the test allowed signature inserts for D_OUT again."
    );

    end_set_aside_period_for_all_set_aside_parent_builds(&case);
    let round_after_set_aside = run_publish_round(&case, None, 2).await;

    assert_eq!(
        (
            round_after_set_aside.failure.is_none(),
            list_signed_dependency_outputs_in_cache(&case, c1).await,
        ),
        (true, vec![B_OUT, D_OUT, F_OUT, I_OUT, N_OUT]),
        "The next round tried parent build C again and signed D_OUT after the end of the set-aside period."
    );
    case.zero_drift("after the set-aside period").await;
    case.finish().await;
}

#[tokio::test]
async fn output_signed_before_signing_cache_subscription_disabled_is_served_and_later_output_is_not()
 {
    let e1 = EvaluationId::now_v7();
    let binary_cache_id_with_signing_enabled = CacheId::now_v7();
    let case = Case::start(
        chain(e1)
            .into_cache(binary_cache_id_with_signing_enabled)
            .keyed(Case::key()),
    )
    .await;
    let producer = case.make_worker_producer_for_evaluation_project(e1).await;
    case.commit_nar(commit_of_b(producer.clone())).await;
    signed(
        &case,
        B_OUT,
        binary_cache_id_with_signing_enabled,
        "The cache signed B_OUT before signing stopped.",
    )
    .await;

    project_cache::Entity::update_many()
        .col_expr(project_cache::Column::SignOutputs, Expr::value(false))
        .filter(project_cache::Column::Cache.eq(binary_cache_id_with_signing_enabled))
        .exec(&case.db.connection())
        .await
        .expect("The subscription can stop signing outputs.");
    case.commit_nar(NarCommit {
        producer,
        ..commit_of(A_OUT, "a", &[(B_OUT, "b")])
    })
    .await;

    null_row(
        &case,
        A_OUT,
        binary_cache_id_with_signing_enabled,
        "A_OUT can get a row with a null signature after signing stops.",
    )
    .await;
    let mut store_path_hash_to_served_content_variant = gradient_db::store_path_content_variant_reader_choice::get_content_variants_served_by_binary_cache_for_store_path_hashes(
        &case.db.connection(),
        binary_cache_id_with_signing_enabled,
        &[B_OUT.to_owned(), A_OUT.to_owned()],
    )
    .await
    .expect("The test can read the variants that a binary cache served.");
    assert!(
        store_path_hash_to_served_content_variant
            .remove(B_OUT)
            .is_some(),
        "The cache subscription signed B_OUT before the subscription set `sign_outputs` to false. The cache can still serve B_OUT."
    );
    assert!(
        store_path_hash_to_served_content_variant
            .remove(A_OUT)
            .is_none(),
        "A_OUT has no signature because its commit happened after signing stopped. The binary cache does not serve A_OUT."
    );
    case.zero_drift("after signing stopped").await;
    case.finish().await;
}

#[tokio::test]
async fn test_first_publish_page_of_two_skips_five_parent_builds_whose_dependency_outputs_are_only_in_unread_cache_and_signs_parent_build_h()
 {
    let (cache_owner_project_evaluation_id, stored_dependency_outputs_owner_evaluation_id, e1) = (
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
    );
    let (other_project_cache_id_not_read_by_signing_project, c1) =
        (CacheId::now_v7(), CacheId::now_v7());
    let seed = shape()
        .evaluation(cache_owner_project_evaluation_id)
        .evaluation_status(EvaluationStatus::Completed)
        .into_cache(other_project_cache_id_not_read_by_signing_project)
        .cached_path(B_OUT, "b")
        .mark_uploaded_by_user()
        .cached_path(D_OUT, "d")
        .mark_uploaded_by_user()
        .cached_path(F_OUT, "f")
        .mark_uploaded_by_user()
        .cached_path(N_OUT, "n")
        .mark_uploaded_by_user()
        .cached_path(Z_OUT, "z")
        .mark_uploaded_by_user()
        .evaluation(stored_dependency_outputs_owner_evaluation_id)
        .cached_path(I_OUT, "i")
        .evaluation(e1)
        .evaluation_status(EvaluationStatus::Building)
        .into_cache(c1)
        .keyed(Case::key());
    let seed = [
        ((A, A_OUT), (B, B_OUT)),
        ((C, C_OUT), (D, D_OUT)),
        ((G, G_OUT), (F, F_OUT)),
        ((M, M_OUT), (N, N_OUT)),
        ((X, X_OUT), (Z, Z_OUT)),
        ((H, H_OUT), (I, I_OUT)),
    ]
    .into_iter()
    .fold(seed, |seed, (waiting, completed)| {
        add_waiting_build_that_depends_on_completed_build(
            seed,
            waiting,
            completed,
            BuildStatus::Queued,
        )
    });
    let case = Case::start(seed).await;
    let mut waiting_derivations = Vec::new();
    for path in [A, C, G, M, X, H] {
        waiting_derivations.push(case.sb(path).await.derivation);
    }
    assert!(
        waiting_derivations.is_sorted(),
        "Five pending builds store their dependency outputs only as uploads in a cache that the signing project does not read. They sort before pending build H, whose dependency outputs are readable."
    );

    publish_one_parent_build_page(&case, None, 2).await;

    assert!(
        case.signatures(I_OUT)
            .await
            .iter()
            .any(|row| row.cache == c1 && row.signature.is_some()),
        "Only parent builds with a copy of a dependency output that the project can publish appear on the first page for limit 2. Build H is on that page. The five builds before it are not. Build H's I_OUT copy is in the signing cache after the publish step."
    );
    let mut signing_cache_rows_with_null_signature = Vec::new();
    for hash in [B_OUT, D_OUT, F_OUT, N_OUT, Z_OUT] {
        signing_cache_rows_with_null_signature.extend(
            case.signatures(hash)
                .await
                .into_iter()
                .filter(|row| row.cache == c1 && row.signature.is_none()),
        );
    }
    assert_eq!(
        signing_cache_rows_with_null_signature,
        Vec::new(),
        "The first page doesn't write a row with an empty signature to the signing cache for dependency outputs. Those outputs exist only as uploads in a cache that the signing project does not read."
    );
    case.finish().await;
}

#[tokio::test]
async fn publish_pass_signs_only_readable_dependency_output_and_writes_no_row_for_unread_upload_or_worker_built_path_reference_to_unread_upload()
 {
    let (cache_owner_project_evaluation_id, stored_dependency_outputs_owner_evaluation_id, e1) = (
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
    );
    let (other_project_cache_id_not_read_by_signing_project, c1) =
        (CacheId::now_v7(), CacheId::now_v7());
    let seed = shape()
        .evaluation(cache_owner_project_evaluation_id)
        .evaluation_status(EvaluationStatus::Completed)
        .into_cache(other_project_cache_id_not_read_by_signing_project)
        .cached_path(B_OUT, "b")
        .mark_uploaded_by_user()
        .cached_path(R_OUT, "r")
        .mark_uploaded_by_user()
        .evaluation(stored_dependency_outputs_owner_evaluation_id)
        .cached_path(D_OUT, "d")
        .references(&[(R_OUT, "r")])
        .cached_path(F_OUT, "f")
        .evaluation(e1)
        .evaluation_status(EvaluationStatus::Building)
        .into_cache(c1)
        .keyed(Case::key());
    let seed = [
        ((A, A_OUT), (B, B_OUT), BuildStatus::Queued),
        ((C, C_OUT), (D, D_OUT), BuildStatus::Queued),
        ((G, G_OUT), (F, F_OUT), BuildStatus::Queued),
    ]
    .into_iter()
    .fold(seed, |seed, (waiting, completed, waiting_status)| {
        add_waiting_build_that_depends_on_completed_build(seed, waiting, completed, waiting_status)
    });
    let case = Case::start(seed).await;
    let load_cached_path_signature_rows_from_signing_cache_for_store_path_hash =
        async |hash: &str| {
            case.signatures(hash)
                .await
                .into_iter()
                .filter(|row| row.cache == c1)
                .collect::<Vec<_>>()
        };

    run_publish_round_and_assert_success(&case, 64).await;

    assert_eq!(
        load_cached_path_signature_rows_from_signing_cache_for_store_path_hash(B_OUT).await,
        Vec::new(),
        "The signing project can't read uploads of input B in another project's cache. This path does not write a row for B to the signing cache."
    );
    assert_eq!(
        (
            load_cached_path_signature_rows_from_signing_cache_for_store_path_hash(R_OUT).await,
            load_cached_path_signature_rows_from_signing_cache_for_store_path_hash(D_OUT).await,
        ),
        (Vec::new(), Vec::new()),
        "The worker built input D, but D referenced R. R existed only as an upload in a cache that the signing project didn't read. This path wrote no row for R or D to the signature cache."
    );
    assert!(
        load_cached_path_signature_rows_from_signing_cache_for_store_path_hash(F_OUT)
            .await
            .iter()
            .any(|row| row.signature.is_some()),
        "This path can sign input F that the worker built and write the signature to the signature cache."
    );
    case.finish().await;
}

async fn start_case_with_queued_parent_build_using_signing_cache_waiting_on_dependency_output_from_other_project(
    stored_dependency_outputs_owner_evaluation_id: EvaluationId,
    c1: CacheId,
) -> Case {
    let seed = shape()
        .evaluation(stored_dependency_outputs_owner_evaluation_id)
        .cached_path(F_OUT, "f")
        .evaluation(EvaluationId::now_v7())
        .evaluation_status(EvaluationStatus::Building)
        .into_cache(c1)
        .keyed(Case::key());
    let seed = add_waiting_build_that_depends_on_completed_build(
        seed,
        (G, G_OUT),
        (F, F_OUT),
        BuildStatus::Queued,
    );
    Case::start(seed).await
}

async fn commit_dependency_output_uploaded_by_other_project_worker(
    case: &Case,
    stored_dependency_outputs_owner_evaluation_id: EvaluationId,
) -> anyhow::Result<()> {
    let producer = case
        .build_worker_stored_content_producer_for_project_or_insert_inactive_registration(
            case.db
                .project_of(stored_dependency_outputs_owner_evaluation_id)
                .await,
        )
        .await;
    case.commit_nar_result(NarCommit {
        producer,
        ..nar(F_OUT, "f")
    })
    .await
    .map(drop)
}

async fn assert_signature_sweep_wake_arrives_within_ten_seconds(case: &Case, message: &str) {
    assert!(
        tokio::time::timeout(
            Duration::from_secs(10),
            case.ctx.wake.run_signature_sweep_now.notified(),
        )
        .await
        .is_ok(),
        "{message}"
    );
}

async fn consume_signature_sweep_wake_sent_before_step_under_test(case: &Case) {
    let _ = tokio::time::timeout(
        Duration::from_millis(10),
        case.ctx.wake.run_signature_sweep_now.notified(),
    )
    .await;
}

pub(crate) async fn run_publish_round(
    case: &Case,
    start: Option<DerivationId>,
    parent_builds_per_page: usize,
) -> crate::DependencyOutputPublishRoundResult {
    case.graph
        .run_publish_round_for_signed_dependency_outputs_of_parent_builds_in_recent_evaluations_until_deadline(
            start,
            parent_builds_per_page,
            std::time::Instant::now() + Duration::from_secs(60),
        )
        .await
}

pub(crate) async fn publish_one_parent_build_page(
    case: &Case,
    start: Option<DerivationId>,
    parent_builds_per_page: usize,
) -> crate::DependencyOutputPublishRoundResult {
    case.graph
        .run_publish_round_for_signed_dependency_outputs_of_parent_builds_in_recent_evaluations_until_deadline(
            start,
            parent_builds_per_page,
            std::time::Instant::now(),
        )
        .await
}

pub(crate) async fn run_publish_round_and_assert_success(
    case: &Case,
    parent_builds_per_page: usize,
) {
    let round = run_publish_round(case, None, parent_builds_per_page).await;
    assert!(
        round.failure.is_none(),
        "The publish round failed: {:?}",
        round.failure
    );
}

#[tokio::test]
async fn dependency_output_commit_writes_no_signature_row_queues_parent_build_wakes_signature_sweep_and_publish_pass_signs_output()
 {
    let (stored_dependency_outputs_owner_evaluation_id, c1) =
        (EvaluationId::now_v7(), CacheId::now_v7());
    let case = start_case_with_queued_parent_build_using_signing_cache_waiting_on_dependency_output_from_other_project(
        stored_dependency_outputs_owner_evaluation_id,
        c1,
    )
    .await;
    consume_signature_sweep_wake_sent_before_step_under_test(&case).await;

    commit_dependency_output_uploaded_by_other_project_worker(
        &case,
        stored_dependency_outputs_owner_evaluation_id,
    )
    .await
    .expect("The commit of F_OUT is successful.");

    assert_eq!(
        case.queue_state(G).await,
        (BuildStatus::Queued, 0),
        "No blocking dependencies exist for parent build G right after the commit. G is in Queued state."
    );
    assert!(
        case.signatures(F_OUT)
            .await
            .iter()
            .all(|row| row.cache != c1),
        "The commit transaction didn't write the signature row for F_OUT to the signing cache for the G project."
    );
    assert_signature_sweep_wake_arrives_within_ten_seconds(
        &case,
        "That commit sent a wake-up to the signature sweep within 10 seconds.",
    )
    .await;
    run_publish_round_and_assert_success(&case, 64).await;
    assert!(
        case.signatures(F_OUT)
            .await
            .iter()
            .any(|row| row.cache == c1 && row.signature.is_some()),
        "One publish pass wrote the signed row for F_OUT to the signing cache."
    );
    case.finish().await;
}

#[tokio::test]
async fn dependency_commit_wakes_signature_sweep_while_cache_inserts_fail_then_publish_signs_after_inserts_allowed()
 {
    let (stored_dependency_outputs_owner_evaluation_id, c1) =
        (EvaluationId::now_v7(), CacheId::now_v7());
    let case = start_case_with_queued_parent_build_using_signing_cache_waiting_on_dependency_output_from_other_project(
        stored_dependency_outputs_owner_evaluation_id,
        c1,
    )
    .await;
    let signing_cache_insert_trigger_sql = format!(
        "CREATE FUNCTION fail_signing_cache_signature_insert() RETURNS trigger LANGUAGE plpgsql AS \
         $$ BEGIN RAISE EXCEPTION 'signing cache signature insert refused'; END $$; \
         CREATE TRIGGER fail_signing_cache_signature_insert BEFORE INSERT ON cached_path_signature \
         FOR EACH ROW WHEN (NEW.cache = '{c1}') EXECUTE FUNCTION fail_signing_cache_signature_insert()"
    );
    case.db
        .connection()
        .execute_unprepared(&signing_cache_insert_trigger_sql)
        .await
        .expect("the trigger is created");
    consume_signature_sweep_wake_sent_before_step_under_test(&case).await;

    commit_dependency_output_uploaded_by_other_project_worker(
        &case,
        stored_dependency_outputs_owner_evaluation_id,
    )
    .await
    .expect("A failed insert of a signature into the signing cache of the waiting build's project can't block the commit of F_OUT.");

    assert!(
        case.signatures(F_OUT)
            .await
            .iter()
            .all(|row| row.cache != c1),
        "The failed signing step left no signature line for F_OUT in the signing cache after the commit."
    );
    assert_signature_sweep_wake_arrives_within_ten_seconds(
        &case,
        "Every signature insert into the signing cache failed. The commit still started the signature sweep within 10 seconds.",
    )
    .await;
    let failing_round = run_publish_round(&case, None, 64).await;
    assert_eq!(
        (
            failing_round.failure.is_none(),
            failing_round
                .parent_build_ids_set_aside_in_publish_round
                .len()
        ),
        (true, 1),
        "Each signature insert into the signing cache failed. The publish pass set the parent build aside. The round reported no failure."
    );
    assert!(
        case.signatures(F_OUT)
            .await
            .iter()
            .all(|row| row.cache != c1),
        "A failed publish pass left no signature row for F_OUT in the signing cache."
    );
    case.db
        .connection()
        .execute_unprepared(
            "DROP TRIGGER fail_signing_cache_signature_insert ON cached_path_signature; \
             DROP FUNCTION fail_signing_cache_signature_insert()",
        )
        .await
        .expect("the trigger is dropped");
    end_set_aside_period_for_all_set_aside_parent_builds(&case);
    run_publish_round_and_assert_success(&case, 64).await;
    assert!(
        case.signatures(F_OUT)
            .await
            .iter()
            .any(|row| row.cache == c1 && row.signature.is_some()),
        "The trigger dropped. A periodic signing pass can sign F_OUT into the signing cache once."
    );
    case.finish().await;
}

type WaitingParentAndCompletedDependencyBuildStorePaths =
    ((&'static str, &'static str), (&'static str, &'static str));

const FIXTURE_PARENT_BUILDS: [WaitingParentAndCompletedDependencyBuildStorePaths; 5] = [
    ((A, A_OUT), (B, B_OUT)),
    ((C, C_OUT), (D, D_OUT)),
    ((G, G_OUT), (F, F_OUT)),
    ((H, H_OUT), (I, I_OUT)),
    ((M, M_OUT), (N, N_OUT)),
];

async fn start_case_with_queued_parent_builds_and_signing_cache(
    parent_build_count: usize,
) -> (Case, CacheId) {
    let c1 = CacheId::now_v7();
    let pairs = &FIXTURE_PARENT_BUILDS[..parent_build_count];
    let seed = pairs
        .iter()
        .fold(
            shape().evaluation(EvaluationId::now_v7()),
            |seed, (_, (_, completed_output))| seed.cached_path(completed_output, completed_output),
        )
        .evaluation(EvaluationId::now_v7())
        .evaluation_status(EvaluationStatus::Building)
        .into_cache(c1)
        .keyed(Case::key());
    let seed = pairs.iter().fold(seed, |seed, &(waiting, completed)| {
        add_waiting_build_that_depends_on_completed_build(
            seed,
            waiting,
            completed,
            BuildStatus::Queued,
        )
    });
    (Case::start(seed).await, c1)
}

async fn list_signed_dependency_outputs_in_cache(case: &Case, cache: CacheId) -> Vec<&'static str> {
    let mut signed_dependency_outputs = Vec::new();
    for (_, (_, completed_output)) in FIXTURE_PARENT_BUILDS {
        if case.find_cached_path(completed_output).await.is_some()
            && case
                .signatures(completed_output)
                .await
                .iter()
                .any(|row| row.cache == cache && row.signature.is_some())
        {
            signed_dependency_outputs.push(completed_output);
        }
    }
    signed_dependency_outputs
}

async fn signature_sweep_wake_arrives_within_500_milliseconds(case: &Case) -> bool {
    tokio::time::timeout(
        Duration::from_millis(500),
        case.ctx.wake.run_signature_sweep_now.notified(),
    )
    .await
    .is_ok()
}

#[tokio::test]
async fn publish_round_with_no_rows_to_sign_sends_no_insert_message_to_graph_writer() {
    let (case, c1) = start_case_with_queued_parent_builds_and_signing_cache(1).await;

    let first_round = run_publish_round(&case, None, 2).await;
    assert_eq!(
        (
            first_round.failure.is_none(),
            first_round.insert_messages_sent_to_graph_writer,
            list_signed_dependency_outputs_in_cache(&case, c1).await,
        ),
        (true, 1, vec![B_OUT]),
        "The first round signed the inputs of build A with one insert message."
    );

    let second_round = run_publish_round(&case, None, 2).await;
    assert_eq!(
        (
            second_round.failure.is_none(),
            second_round.insert_messages_sent_to_graph_writer
        ),
        (true, 0),
        "Round 2 had nothing to sign and sent no message to the graph writer."
    );
    case.finish().await;
}

#[tokio::test]
async fn one_publish_round_signs_dependency_outputs_for_parent_builds_on_several_pages() {
    let (case, c1) = start_case_with_queued_parent_builds_and_signing_cache(5).await;

    let round = run_publish_round(&case, None, 2).await;

    assert_eq!(
        (
            round.failure.is_none(),
            round.stopped_at_deadline_with_parent_builds_remaining,
            round.insert_messages_sent_to_graph_writer,
            list_signed_dependency_outputs_in_cache(&case, c1)
                .await
                .len(),
        ),
        (true, false, 3, 5),
        "The round signed the inputs of all five parent builds across 3 pages, with a page size of 2 parent builds."
    );
    case.finish().await;
}

#[tokio::test]
async fn publish_round_deadline_keeps_cursor_requests_signature_sweep_once_and_next_round_signs_remaining_once()
 {
    let (case, c1) = start_case_with_queued_parent_builds_and_signing_cache(5).await;
    consume_signature_sweep_wake_sent_before_step_under_test(&case).await;

    let stopped_round = publish_one_parent_build_page(&case, None, 2).await;
    assert_eq!(
        (
            stopped_round.failure.is_none(),
            stopped_round.stopped_at_deadline_with_parent_builds_remaining,
            stopped_round.last_handled_parent_build_id.is_some(),
            stopped_round.insert_messages_sent_to_graph_writer,
            list_signed_dependency_outputs_in_cache(&case, c1)
                .await
                .len(),
        ),
        (true, true, true, 1, 2),
        "A round is past its deadline. It stopped after one full page of two parent builds. It kept the cursor."
    );
    assert_signature_sweep_wake_arrives_within_ten_seconds(
        &case,
        "Rounds that stop at their deadline request one more run of the signature sweep.",
    )
    .await;

    let next_round = run_publish_round(&case, stopped_round.last_handled_parent_build_id, 2).await;
    assert_eq!(
        (
            next_round.failure.is_none(),
            next_round.insert_messages_sent_to_graph_writer,
            list_signed_dependency_outputs_in_cache(&case, c1)
                .await
                .len(),
        ),
        (true, 2, 5),
        "The next round started at the saved cursor. It signed the inputs of the remaining three parent builds and sent two insert messages. It signed nothing for the first two."
    );
    case.finish().await;
}

#[tokio::test]
async fn publish_round_without_parent_builds_after_deadline_does_not_wake_signature_sweep() {
    let (case, c1) = start_case_with_queued_parent_builds_and_signing_cache(1).await;
    consume_signature_sweep_wake_sent_before_step_under_test(&case).await;

    let short_page_round = publish_one_parent_build_page(&case, None, 2).await;
    let empty_page_round =
        publish_one_parent_build_page(&case, short_page_round.last_handled_parent_build_id, 2)
            .await;

    assert_eq!(
        (
            short_page_round.failure.is_none(),
            short_page_round.stopped_at_deadline_with_parent_builds_remaining,
            empty_page_round.failure.is_none(),
            empty_page_round.stopped_at_deadline_with_parent_builds_remaining,
            list_signed_dependency_outputs_in_cache(&case, c1).await,
            signature_sweep_wake_arrives_within_500_milliseconds(&case).await,
        ),
        (true, false, true, false, vec![B_OUT], false),
        "Both publish rounds ran past their deadline. The first read a short page, with fewer parent builds than the page size. The next read an empty page. No parent builds were left. Neither round requested another run of the signature sweep."
    );
    case.finish().await;
}

#[tokio::test]
async fn signed_dependency_output_row_carries_cache_signing_time_public_key() {
    let (case, c1) = start_case_with_queued_parent_builds_and_signing_cache(1).await;
    let parent_builds =
        crate::claims::read_page_of_parent_builds_with_dependency_outputs_to_publish(
            &case.ctx.worker_db,
            DerivationId::nil(),
            DerivationId::new(uuid::Uuid::max()),
            2,
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

    let cache_public_key = gradient_entity::cache::Entity::find_by_id(c1)
        .one(&case.ctx.worker_db)
        .await
        .expect("Cache Read")
        .expect("The cache exists")
        .public_key;
    assert_eq!(
        signed_dependency_output_rows
            .iter()
            .map(|row| (row.cache, row.cache_public_key_at_signing.clone()))
            .collect::<Vec<_>>(),
        vec![(c1, Some(cache_public_key))],
        "Signed rows for build A's input store the public key of cache C1 at signing time."
    );
    case.finish().await;
}

#[tokio::test]
async fn page_read_and_signing_select_return_while_graph_writer_is_down_only_insert_waits() {
    let (mut case, c1) = start_case_with_queued_parent_builds_and_signing_cache(1).await;
    case.writer
        .kill_and_wait(None)
        .await
        .expect("The test killed the graph writer.");

    let parent_builds = tokio::time::timeout(
        Duration::from_secs(5),
        crate::claims::read_page_of_parent_builds_with_dependency_outputs_to_publish(
            &case.ctx.worker_db,
            DerivationId::nil(),
            DerivationId::new(uuid::Uuid::max()),
            2,
        ),
    )
    .await
    .expect("A page read came back with the graph writer stopped.")
    .expect("Read the parent build page");
    let signed_dependency_output_rows = tokio::time::timeout(
        Duration::from_secs(5),
        crate::claims::select_and_sign_dependency_outputs_of_parent_build_page(
            &case.ctx,
            &parent_builds,
        ),
    )
    .await
    .expect("Select and signing return while the graph writer stays stopped.")
    .expect("Select dependency outputs and sign them.");
    assert_eq!(
        (parent_builds.len(), signed_dependency_output_rows.len()),
        (1, 1),
        "Page read found build A with the graph writer down. Select signed its input."
    );

    let graph_writer_restart_task = async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let writer = case
            .graph
            .spawn(case.ctx.clone(), None, None)
            .await
            .expect("Restart the graph writer.");
        (writer, std::time::Instant::now())
    };
    let insert_task = async {
        let inserted = case
            .graph
            .transition(
                crate::messages::Transition::SignStoredInputsIntoReadWriteBinaryCache {
                    signed_dependency_output_rows_to_insert: signed_dependency_output_rows,
                },
            )
            .await;
        (inserted, std::time::Instant::now())
    };
    let ((writer, restarted_at), (inserted, inserted_at)) =
        tokio::join!(graph_writer_restart_task, insert_task);
    case.writer = writer;

    inserted.expect("The insert message reaches the graph writer after restart.");
    assert_eq!(
        (
            inserted_at >= restarted_at,
            list_signed_dependency_outputs_in_cache(&case, c1).await,
        ),
        (true, vec![B_OUT]),
        "Only the insert message waited for the graph writer to return. The graph writer then wrote the signed input."
    );
    case.finish().await;
}

gradient_db::sql! {
    READ_STATEMENT_TIMEOUT_SETTING_QUERY = "SELECT current_setting('statement_timeout') AS statement_timeout",
        params = [];
}

#[tokio::test]
async fn test_page_read_5s_signing_select_60s_statement_timeouts() {
    let case = Case::start(shape().evaluation(EvaluationId::now_v7())).await;
    let mut statement_timeouts = Vec::new();
    for statement_timeout in [
        crate::claims::PARENT_BUILD_PAGE_READ_STATEMENT_TIMEOUT,
        crate::claims::DEPENDENCY_OUTPUT_SIGNING_SELECT_STATEMENT_TIMEOUT,
    ] {
        let transaction = crate::claims::begin_read_only_transaction_and_set_statement_timeout(
            &case.ctx.worker_db,
            statement_timeout,
        )
        .await
        .expect("Read-only transaction starts");
        let row = transaction
            .query_one_raw(READ_STATEMENT_TIMEOUT_SETTING_QUERY.stmt())
            .await
            .expect("Read the setting")
            .expect("The settings row is present.");
        statement_timeouts.push(
            row.try_get::<String>("", "statement_timeout")
                .expect("The setting is text."),
        );
        transaction
            .rollback()
            .await
            .expect("End of read-only transaction");
    }

    assert_eq!(
        statement_timeouts,
        ["5s", "1min"],
        "The two queries run with a 5 second statement_timeout for the page read and a 60 second statement_timeout for the publish select."
    );
    case.finish().await;
}

async fn sign_one_parent_build_page_dependency_outputs_without_insert(
    case: &Case,
) -> Vec<crate::claims::SignatureRowForGraphWriterInsert> {
    let parent_builds =
        crate::claims::read_page_of_parent_builds_with_dependency_outputs_to_publish(
            &case.ctx.worker_db,
            DerivationId::nil(),
            DerivationId::new(uuid::Uuid::max()),
            2,
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
    assert!(
        !signed_dependency_output_rows.is_empty(),
        "Page read and select sign the inputs of build A."
    );
    signed_dependency_output_rows
}

async fn send_signed_dependency_output_rows_to_graph_writer(
    case: &Case,
    signed_dependency_output_rows: Vec<crate::claims::SignatureRowForGraphWriterInsert>,
) {
    case.graph
        .transition(
            crate::messages::Transition::SignStoredInputsIntoReadWriteBinaryCache {
                signed_dependency_output_rows_to_insert: signed_dependency_output_rows,
            },
        )
        .await
        .expect("Make the insert message reach the graph writer.");
}

async fn read_dependency_output_signature_rows(
    case: &Case,
    dependency_output: &str,
    cache: CacheId,
) -> Vec<Option<Vec<u8>>> {
    case.signatures(dependency_output)
        .await
        .into_iter()
        .filter(|row| row.cache == cache)
        .map(|row| row.signature)
        .collect()
}

async fn start_case_with_parent_waiting_on_uploaded_copy_in_read_cache() -> (Case, CacheId, CacheId)
{
    let (c0, c1) = (CacheId::now_v7(), CacheId::now_v7());
    let seed = shape()
        .evaluation(EvaluationId::now_v7())
        .evaluation_status(EvaluationStatus::Completed)
        .into_cache(c0)
        .cached_path(B_OUT, B_OUT)
        .mark_uploaded_by_user()
        .evaluation(EvaluationId::now_v7())
        .evaluation_status(EvaluationStatus::Building)
        .into_cache(c0)
        .into_cache(c1)
        .keyed(Case::key());
    let seed = add_waiting_build_that_depends_on_completed_build(
        seed,
        (A, A_OUT),
        (B, B_OUT),
        BuildStatus::Queued,
    );
    (Case::start(seed).await, c0, c1)
}

#[tokio::test]
async fn insert_skips_signed_row_when_sign_outputs_turned_off_after_page_read() {
    let (case, c1) = start_case_with_queued_parent_builds_and_signing_cache(1).await;
    let rows_before = read_dependency_output_signature_rows(&case, B_OUT, c1).await;
    let signed_dependency_output_rows =
        sign_one_parent_build_page_dependency_outputs_without_insert(&case).await;

    case.sql(&format!(
        "UPDATE project_cache SET sign_outputs = false WHERE cache = '{c1}'"
    ))
    .await;
    send_signed_dependency_output_rows_to_graph_writer(&case, signed_dependency_output_rows).await;

    assert_eq!(
        read_dependency_output_signature_rows(&case, B_OUT, c1).await,
        rows_before,
        "The graph writer does not save rows for the inputs of build A in cache C1 if sign_outputs is off after the page read."
    );
    case.finish().await;
}

#[tokio::test]
async fn insert_skips_signed_row_when_read_access_is_lost_after_page_read() {
    let (case, c0, c1) = start_case_with_parent_waiting_on_uploaded_copy_in_read_cache().await;
    let rows_before = read_dependency_output_signature_rows(&case, B_OUT, c1).await;
    let signed_dependency_output_rows =
        sign_one_parent_build_page_dependency_outputs_without_insert(&case).await;

    case.sql(&format!("DELETE FROM project_cache WHERE cache = '{c0}'"))
        .await;
    send_signed_dependency_output_rows_to_graph_writer(&case, signed_dependency_output_rows).await;

    assert_eq!(
        read_dependency_output_signature_rows(&case, B_OUT, c1).await,
        rows_before,
        "The test deletes the subscriptions to cache C0 after the page read. The graph writer does not store the row for the uploaded input of build A in cache C1."
    );
    case.finish().await;
}

async fn insert_unsigned_placeholder_signature_row_for_dependency_output(
    case: &Case,
    dependency_output: &str,
    cache: CacheId,
) {
    case.sql(&format!(
        "INSERT INTO cached_path_signature (id, cached_path, cache, fetch_count, created_at) \
         SELECT uuidv7(), id, '{cache}', 0, (now() AT TIME ZONE 'UTC') \
         FROM cached_path WHERE hash = '{dependency_output}'"
    ))
    .await;
}

#[tokio::test]
async fn unsigned_signature_row_in_target_cache_does_not_publish_uploaded_copy_after_source_cache_read_access_is_lost()
 {
    let (case, c0, c1) = start_case_with_parent_waiting_on_uploaded_copy_in_read_cache().await;
    insert_unsigned_placeholder_signature_row_for_dependency_output(&case, B_OUT, c1).await;
    let rows_before = read_dependency_output_signature_rows(&case, B_OUT, c1).await;
    assert_eq!(rows_before, vec![None]);
    let signed_dependency_output_rows =
        sign_one_parent_build_page_dependency_outputs_without_insert(&case).await;

    case.sql(&format!("DELETE FROM project_cache WHERE cache = '{c0}'"))
        .await;
    send_signed_dependency_output_rows_to_graph_writer(&case, signed_dependency_output_rows).await;
    assert_eq!(
        read_dependency_output_signature_rows(&case, B_OUT, c1).await,
        rows_before,
        "The test removed subscriptions to cache C0 after the page read. The unsigned placeholder for the uploaded input of build A in cache C1 was still unsigned. The graph writer didn't save signatures."
    );

    run_publish_round_and_assert_success(&case, 2).await;
    assert_eq!(
        read_dependency_output_signature_rows(&case, B_OUT, c1).await,
        rows_before,
        "The project lost read access to cache C0. The next full publish round left the placeholder for the input of build A unsigned in cache C1."
    );
    case.finish().await;
}

#[tokio::test]
async fn test_copy_uploaded_to_signing_cache_while_signing_off_is_signed_after_signing_on() {
    let c1 = CacheId::now_v7();
    let seed = shape()
        .evaluation(EvaluationId::now_v7())
        .evaluation_status(EvaluationStatus::Completed)
        .into_cache(c1)
        .cached_path(B_OUT, B_OUT)
        .mark_uploaded_by_user()
        .evaluation(EvaluationId::now_v7())
        .evaluation_status(EvaluationStatus::Building)
        .into_cache(c1)
        .keyed(Case::key());
    let seed = add_waiting_build_that_depends_on_completed_build(
        seed,
        (A, A_OUT),
        (B, B_OUT),
        BuildStatus::Queued,
    );
    let case = Case::start(seed).await;
    case.sql(&format!(
        "UPDATE project_cache SET sign_outputs = false WHERE cache = '{c1}'"
    ))
    .await;
    insert_unsigned_placeholder_signature_row_for_dependency_output(&case, B_OUT, c1).await;

    case.sql(&format!(
        "UPDATE project_cache SET sign_outputs = true WHERE cache = '{c1}'"
    ))
    .await;
    run_publish_round_and_assert_success(&case, 2).await;

    let rows_after_round = read_dependency_output_signature_rows(&case, B_OUT, c1).await;
    assert!(
        rows_after_round.len() == 1 && rows_after_round[0].is_some(),
        "Signing switched on. The next full publish round signed inputs of build A that were uploaded to cache C1 while signing was off."
    );
    case.finish().await;
}

#[tokio::test]
async fn insert_skips_row_signed_with_old_key_after_key_rotation() {
    let (case, c1) = start_case_with_queued_parent_builds_and_signing_cache(1).await;
    let rows_before = read_dependency_output_signature_rows(&case, B_OUT, c1).await;
    let signed_dependency_output_rows =
        sign_one_parent_build_page_dependency_outputs_without_insert(&case).await;
    let old_key_signatures: Vec<Option<Vec<u8>>> = signed_dependency_output_rows
        .iter()
        .map(|row| row.signature.clone())
        .collect();

    case.sql(&format!(
        "UPDATE cache SET private_key = '{key}', public_key = 'rotated public key of C1' WHERE id = '{c1}'",
        key = Case::key(),
    ))
    .await;
    send_signed_dependency_output_rows_to_graph_writer(&case, signed_dependency_output_rows).await;
    assert_eq!(
        read_dependency_output_signature_rows(&case, B_OUT, c1).await,
        rows_before,
        "Key rotations of cache C1 after a page read leave the graph writer with no rows signed with the old key to save."
    );

    run_publish_round_and_assert_success(&case, 2).await;
    let rows_after_next_round = read_dependency_output_signature_rows(&case, B_OUT, c1).await;
    assert!(
        rows_after_next_round.len() == 1
            && rows_after_next_round[0].is_some()
            && !old_key_signatures.contains(&rows_after_next_round[0]),
        "Full rounds after the key rotation sign the input of build A with the new key of cache C1."
    );
    case.finish().await;
}

#[tokio::test]
async fn key_rotation_during_open_insert_transaction_leaves_no_retired_key_signature() {
    let (case, c1) = start_case_with_queued_parent_builds_and_signing_cache(1).await;
    let signed_dependency_output_rows =
        sign_one_parent_build_page_dependency_outputs_without_insert(&case).await;
    let retired_key_signatures: Vec<Option<Vec<u8>>> = signed_dependency_output_rows
        .iter()
        .map(|row| row.signature.clone())
        .collect();
    let insert_transaction = Arc::new(case.ctx.worker_db.begin().await.expect("begin"));
    crate::claims::insert_signed_dependency_output_rows_in_graph_writer_transaction_after_allowed_recheck(
        &case.ctx.in_transaction(Arc::clone(&insert_transaction)),
        signed_dependency_output_rows,
    )
    .await
    .expect("the rows are inserted inside the open transaction");

    let cache = gradient_entity::cache::Entity::find_by_id(c1)
        .one(&case.ctx.web_db)
        .await
        .expect("cache C1 is read")
        .expect("cache C1 exists");
    let web_db = case.ctx.web_db.clone();
    let rotation = Shutdown::new().spawn(async move {
        gradient_db::caches::insert_old_cache_key_row_replace_key_pair_and_move_signatures_to_previous_column(
            &web_db,
            cache,
            "retired public key of C1".to_string(),
            (Case::key(), "rotated public key of C1".to_string()),
        )
        .await
    });
    let lock_waits = format!(
        "SELECT wait_event_type FROM pg_stat_activity WHERE datname = '{}' AND wait_event_type = 'Lock'",
        case.db.name()
    );
    while !rotation.is_finished() && case.db.texts(&lock_waits).await.is_empty() {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Arc::try_unwrap(insert_transaction)
        .expect("no other handle of the insert transaction is left")
        .commit()
        .await
        .expect("the insert commits");
    rotation
        .await
        .expect("the rotation task ends")
        .expect("Commit the rotation");

    let stored = read_dependency_output_signature_rows(&case, B_OUT, c1).await;
    let retired_key_signature_stored = stored
        .iter()
        .any(|signature| signature.is_some() && retired_key_signatures.contains(signature));
    assert!(
        !stored.is_empty() && !retired_key_signature_stored,
        "Key rotations in cache C1 during an open insert transaction leave no signature of the retired key on the input of build A: {stored:?}"
    );
    case.finish().await;
}

#[tokio::test]
async fn key_rotation_during_open_upload_commit_transaction_leaves_no_retired_key_signature() {
    let (e1, c1) = (EvaluationId::now_v7(), CacheId::now_v7());
    let case = Case::start(chain(e1).into_cache(c1).keyed(Case::key())).await;
    let commit = commit_of_b(case.make_worker_producer_for_evaluation_project(e1).await);
    crate::testing::write_commit_compressed_object_to_object_storage(&case.ctx, &commit).await;
    let existing_object_keys = crate::nar::find_object_keys_in_storage_at_batch_start(
        &case.ctx,
        std::slice::from_ref(&commit),
    )
    .await;
    let commit_transaction = Arc::new(case.ctx.worker_db.begin().await.expect("begin"));
    let scoped = case.ctx.in_transaction(Arc::clone(&commit_transaction));
    crate::nar::commit(&scoped, &commit, &existing_object_keys)
        .await
        .expect("the NAR is committed inside the open transaction");
    crate::claims::claim_committed_paths(&scoped, &[B_OUT.to_owned()])
        .await
        .expect("the committed path is claimed inside the open transaction");
    drop(scoped);

    let cache = gradient_entity::cache::Entity::find_by_id(c1)
        .one(&case.ctx.web_db)
        .await
        .expect("cache C1 is read")
        .expect("cache C1 exists");
    let web_db = case.ctx.web_db.clone();
    let rotation = Shutdown::new().spawn(async move {
        gradient_db::caches::insert_old_cache_key_row_replace_key_pair_and_move_signatures_to_previous_column(
            &web_db,
            cache,
            "retired public key of C1".to_string(),
            (Case::key(), "rotated public key of C1".to_string()),
        )
        .await
    });
    let lock_waits = format!(
        "SELECT wait_event_type FROM pg_stat_activity WHERE datname = '{}' AND wait_event_type = 'Lock'",
        case.db.name()
    );
    while !rotation.is_finished() && case.db.texts(&lock_waits).await.is_empty() {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Arc::try_unwrap(commit_transaction)
        .expect("no other handle of the commit transaction is left")
        .commit()
        .await
        .expect("the upload commit transaction commits");
    rotation
        .await
        .expect("the rotation task ends")
        .expect("Commit the rotation");

    let stored = case
        .signature(B_OUT, c1)
        .await
        .expect("the upload commit stored a signature row of B_OUT in cache C1");
    assert!(
        stored.signature.is_none() || case.verifies(&stored, &commit).await,
        "A key rotation of cache C1 during the open upload commit transaction doesn't leave a signature of the retired key on B_OUT in cache C1: {stored:?}"
    );
    case.finish().await;
}

const SHORTER_WAIT_LIMIT_THAN_GRAPH_WRITER_LOCK_WAIT_LIMIT: Duration = Duration::from_secs(5);

async fn begin_transaction_that_rotates_cache_signing_key_and_holds_cache_row_lock(
    case: &Case,
    cache: CacheId,
) -> sea_orm::DatabaseTransaction {
    let cache = gradient_entity::cache::Entity::find_by_id(cache)
        .one(&case.ctx.web_db)
        .await
        .expect("Cache Read")
        .expect("The cache exists");
    let rotation = case.ctx.web_db.begin().await.expect("begin");
    gradient_db::caches::insert_old_cache_key_row_replace_key_pair_and_move_signatures_to_previous_column(
        &rotation,
        cache,
        "retired public key".to_string(),
        (Case::key(), "rotated public key".to_string()),
    )
    .await
    .expect("the rotation updates the cache row inside the open transaction");
    rotation
}

async fn sign_sweep_select_returns_unsigned_row(case: &Case, hash: &str, cache: CacheId) -> bool {
    let pending = format!(
        "SELECT s.id::text FROM cached_path_signature s \
         JOIN cached_path cp ON cp.id = s.cached_path \
         WHERE cp.hash = '{hash}' AND s.cache = '{cache}' AND s.signature IS NULL AND {accepted}",
        accepted = gradient_db::store_path_content_variant_trust::cache_can_sign_content_variant_sql_condition(
            "s.cached_path",
            "s.cache",
        ),
    );
    !case.db.texts(&pending).await.is_empty()
}

#[tokio::test]
async fn record_batch_signs_into_free_cache_without_waiting_for_key_rotation() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let (c1, c2) = (CacheId::now_v7(), CacheId::now_v7());
    let case = Case::start(
        chain(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .without_cache_signing()
            .evaluation(e2)
            .into_cache(c1)
            .into_cache(c2)
            .keyed(Case::key())
            .derivation(B)
            .build_status(BuildStatus::Completed)
            .cached_output(B)
            .derivation(A)
            .build_status(BuildStatus::Queued),
    )
    .await;
    case.seed_signature(B_OUT, c1, None).await;
    case.seed_signature(B_OUT, c2, None).await;
    let rotation =
        begin_transaction_that_rotates_cache_signing_key_and_holds_cache_row_lock(&case, c1).await;

    let walked = tokio::time::timeout(
        SHORTER_WAIT_LIMIT_THAN_GRAPH_WRITER_LOCK_WAIT_LIMIT,
        case.walk_result(Batch {
            derivations: vec![
                crate::messages::EvaluationBatchDerivation::test_batch_entry_from_discovered_derivation(
                    with_attr(with_output(drv(A2, &[B]), A2_OUT), "pkg.a2"),
                ),
            ],
            task: Some(case.db.task_of(e2).await),
            ..batch(e2)
        }),
    )
    .await;
    assert!(
        matches!(walked, Ok(Ok(_))),
        "Record batch that reused B_OUT finished while key rotation of cache C1 was still open: {walked:?}"
    );
    assert_ne!(
        case.evaluation_status(e2).await,
        EvaluationStatus::Failed,
        "Key rotation of cache C1 does not cause evaluation to fail."
    );
    signed(
        &case,
        B_OUT,
        c2,
        "the free cache C2 is signed into in the same batch",
    )
    .await;
    rotation.commit().await.expect("Commit the rotation");

    let left_unsigned = row(&case, B_OUT, c1, "the row of B_OUT in C1 is kept").await;
    assert!(
        left_unsigned.signature.is_none()
            && sign_sweep_select_returns_unsigned_row(&case, B_OUT, c1).await,
        "The row in cache C1 of B_OUT is unsigned. The sign sweep selected that row after rotation committed: {left_unsigned:?}"
    );
    case.finish().await;
}

#[tokio::test]
async fn upload_commit_does_not_wait_for_key_rotation() {
    let (e1, c1) = (EvaluationId::now_v7(), CacheId::now_v7());
    let case = Case::start(chain(e1).into_cache(c1).keyed(Case::key())).await;
    let commit = commit_of_b(case.make_worker_producer_for_evaluation_project(e1).await);
    crate::testing::write_commit_compressed_object_to_object_storage(&case.ctx, &commit).await;
    let rotation =
        begin_transaction_that_rotates_cache_signing_key_and_holds_cache_row_lock(&case, c1).await;

    let committed = tokio::time::timeout(
        SHORTER_WAIT_LIMIT_THAN_GRAPH_WRITER_LOCK_WAIT_LIMIT,
        case.commit_nar_result(commit),
    )
    .await;
    assert!(
        matches!(committed, Ok(Ok(_))),
        "The upload commit of B_OUT finished while the key rotation of cache C1 was still open: {committed:?}"
    );
    rotation.commit().await.expect("Commit the rotation");

    let left_unsigned = row(
        &case,
        B_OUT,
        c1,
        "the upload commit stores a row of B_OUT in C1",
    )
    .await;
    assert!(
        left_unsigned.signature.is_none()
            && sign_sweep_select_returns_unsigned_row(&case, B_OUT, c1).await,
        "The row in cache C1 of B_OUT is unsigned. The sign sweep selected that row after rotation committed: {left_unsigned:?}"
    );
    case.finish().await;
}

async fn start_case_with_queued_parent_build_waiting_on_stored_dependency_output_and_two_caches_with_signing_keys()
-> (Case, CacheId, CacheId) {
    let (c1, c2) = (CacheId::now_v7(), CacheId::now_v7());
    let (waiting, completed) = FIXTURE_PARENT_BUILDS[0];
    let seed = shape()
        .evaluation(EvaluationId::now_v7())
        .cached_path(completed.1, completed.1)
        .evaluation(EvaluationId::now_v7())
        .evaluation_status(EvaluationStatus::Building)
        .into_cache(c1)
        .keyed(Case::key())
        .into_cache(c2)
        .keyed(Case::key());
    let seed = add_waiting_build_that_depends_on_completed_build(
        seed,
        waiting,
        completed,
        BuildStatus::Queued,
    );
    (Case::start(seed).await, c1, c2)
}

#[tokio::test]
async fn publish_round_does_not_wait_for_key_rotation_and_signs_into_cache_not_under_rotation() {
    let (case, c1, c2) =
        start_case_with_queued_parent_build_waiting_on_stored_dependency_output_and_two_caches_with_signing_keys()
            .await;
    let retired_key_signatures: Vec<Option<Vec<u8>>> =
        sign_one_parent_build_page_dependency_outputs_without_insert(&case)
            .await
            .into_iter()
            .map(|row| row.signature)
            .collect();
    let rotation =
        begin_transaction_that_rotates_cache_signing_key_and_holds_cache_row_lock(&case, c1).await;

    let started = std::time::Instant::now();
    let round = tokio::time::timeout(
        SHORTER_WAIT_LIMIT_THAN_GRAPH_WRITER_LOCK_WAIT_LIMIT,
        run_publish_round(&case, None, 2),
    )
    .await
    .expect("Complete the publish round while the key rotation of cache C1 is still open.");
    let lock_wait_limit =
        crate::writer::GRAPH_WRITER_SIGNED_DEPENDENCY_OUTPUT_ROW_INSERT_TRANSACTION_LIMITS
            .statement_lock_wait_timeout;
    assert!(
        started.elapsed() < lock_wait_limit
            && round.failure.is_none()
            && round.failed_page_count == 0
            && round.parent_build_ids_set_aside_in_publish_round.is_empty(),
        "The publish round is answered before the lock wait limit of {lock_wait_limit:?} without a failed page or a parent build set aside while a key rotation of cache C1 is open: took {:?}, failed pages {}, set aside {:?}, failure {:?}",
        started.elapsed(),
        round.failed_page_count,
        round.parent_build_ids_set_aside_in_publish_round,
        round.failure
    );
    assert!(
        read_dependency_output_signature_rows(&case, B_OUT, c1)
            .await
            .iter()
            .all(Option::is_none),
        "The publish round does not write signatures for the inputs of build A to cache C1 while the key rotation of cache C1 is incomplete."
    );
    signed(
        &case,
        B_OUT,
        c2,
        "the free cache C2 is signed into in the same round",
    )
    .await;
    rotation.commit().await.expect("Commit the rotation");

    run_publish_round_and_assert_success(&case, 2).await;
    let rows_after_rotation = read_dependency_output_signature_rows(&case, B_OUT, c1).await;
    assert!(
        rows_after_rotation.len() == 1
            && rows_after_rotation[0].is_some()
            && !retired_key_signatures.contains(&rows_after_rotation[0]),
        "The next round after the rotation commit signed the inputs of build A in cache C1 with the new key: {rows_after_rotation:?}"
    );
    case.finish().await;
}

#[tokio::test]
async fn insert_keeps_signature_signed_by_another_writer_after_page_read() {
    let (case, c1) = start_case_with_queued_parent_builds_and_signing_cache(1).await;
    let signed_dependency_output_rows =
        sign_one_parent_build_page_dependency_outputs_without_insert(&case).await;
    send_signed_dependency_output_rows_to_graph_writer(
        &case,
        signed_dependency_output_rows.clone(),
    )
    .await;
    case.sql(&format!(
        "UPDATE cached_path_signature SET signature = '\\x0102'::bytea WHERE cache = '{c1}'"
    ))
    .await;

    send_signed_dependency_output_rows_to_graph_writer(&case, signed_dependency_output_rows).await;

    assert_eq!(
        read_dependency_output_signature_rows(&case, B_OUT, c1).await,
        vec![Some(vec![1, 2])],
        "A row that a pass read before another writer signed it did not change the stored signature."
    );
    case.finish().await;
}
