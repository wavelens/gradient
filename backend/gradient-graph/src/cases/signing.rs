/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use gradient_entity::build::BuildStatus;
use gradient_entity::cached_path_signature;
use gradient_entity::ids::{CacheId, EvaluationId};
use gradient_entity::{MappingContext, RotatedHashMapping};
use gradient_fixtures::shape::shape;

use super::{
    A, A_OUT, A2, A2_OUT, B, B_OUT, Case, SOURCE, X_OUT, batch, chain, drv, nar, with_attr,
    with_output,
};
use crate::messages::{NarCommit, SignTargets};
use crate::test_ctx::{Batch, BatchReport};

fn commit_of_b(targets: SignTargets) -> NarCommit {
    NarCommit {
        references: vec![format!("{B_OUT}-b")],
        targets,
        ..nar(B_OUT, "b")
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
        derivations: vec![with_attr(with_output(drv(path, &[B]), output), attr)],
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
    let commit = NarCommit {
        built_by_worker: true,
        ..commit_of_b(case.project_caches(e1).await)
    };

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

    case.commit_nar(commit_of_b(SignTargets::Cache(c2))).await;

    null_row(&case, B_OUT, c2, "after 1: C2 null, a private path").await;
    no_row(&case, B_OUT, c1, "after 1: no row in C1").await;
    case.zero_drift("step 1").await;

    case.commit_nar(nar(SOURCE, "source")).await;

    let rows = case.signatures(SOURCE).await;
    assert!(rows.is_empty(), "after 2: no row of the source: {rows:?}");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn k03_a_private_path_gets_a_null_row_and_a_signing_task_among_its_producers_signs_it() {
    for signing_producer in [false, true] {
        let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
        let c1 = CacheId::now_v7();
        let private = chain(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .without_cache_signing();

        let case = Case::start(if signing_producer {
            private.evaluation(e2).derivation(B).job()
        } else {
            private
        })
        .await;

        let commit = commit_of_b(case.project_caches(e1).await);
        case.commit_nar(commit).await;

        if signing_producer {
            signed(&case, B_OUT, c1, "b after 1: C1 non-null").await;
        } else {
            null_row(&case, B_OUT, c1, "a after 1: C1 null").await;
        }

        case.zero_drift("step 1").await;
        case.finish().await;
    }
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

    let commit = |targets| NarCommit {
        file_hash: "fh".into(),
        built_by_worker: true,
        ..commit_of_b(targets)
    };

    let report = case.commit_nar(commit(SignTargets::Cache(c1))).await;

    assert_eq!(report.signed, vec![c1], "after 1: report.signed");
    let targeted = signed(&case, B_OUT, c1, "after 1: C1 non-null").await;
    let claimed = signed(&case, B_OUT, c2, "after 1: C2 signed by the claim").await;
    case.zero_drift("step 1").await;

    let report = case.commit_nar(commit(SignTargets::Cache(c1))).await;

    assert!(!report.created, "after 2: report.created == false");
    assert_eq!(report.signed, vec![c1], "after 2: report.signed");
    unchanged(&case, B_OUT, c1, &targeted, "after 2: C1 as after 1").await;
    let rows = case.signatures(B_OUT).await.len();
    assert_eq!(rows, 2, "after 2: the target row and the claimed row");
    case.zero_drift("step 2").await;

    let report = case.commit_nar(commit(SignTargets::Cache(c2))).await;

    assert_eq!(report.signed, vec![c2], "after 3: only the new target");
    unchanged(&case, B_OUT, c2, &claimed, "after 3: C2 unchanged").await;
    unchanged(&case, B_OUT, c1, &targeted, "after 3: C1 unchanged").await;
    case.zero_drift("step 3").await;
    case.finish().await;
}

#[tokio::test]
async fn k05_a_recommit_with_other_content_changes_nothing_on_the_row_or_its_signatures() {
    let (e1, c1, c2) = (EvaluationId::now_v7(), CacheId::now_v7(), CacheId::now_v7());
    let case = Case::start(
        chain(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .into_cache(c2)
            .keyed(Case::key()),
    )
    .await;

    let first = NarCommit {
        file_hash: "fh1".into(),
        ..commit_of_b(SignTargets::None)
    };

    case.commit_nar(first.clone()).await;

    let stored = case.cached_path(B_OUT).await;
    let in_c1 = signed_over(&case, B_OUT, c1, &first, "after 1: C1 claimed over fh1").await;
    let in_c2 = signed_over(&case, B_OUT, c2, &first, "after 1: C2 claimed over fh1").await;
    case.zero_drift("step 1").await;

    let by_worker = case
        .commit_nar(NarCommit {
            file_hash: "fh2".into(),
            references: vec![format!("{B_OUT}-b"), format!("{X_OUT}-x")],
            built_by_worker: true,
            ..commit_of_b(SignTargets::Cache(c1))
        })
        .await;

    assert_eq!(by_worker.signed, vec![c1], "after 2: a worker signs");
    let (row, references) = (case.cached_path(B_OUT).await, format!("{B_OUT}-b"));
    assert_eq!(row.file_hash.as_deref(), Some("fh1"), "after 2: file_hash");
    assert_eq!(row.references, Some(references), "after 2: references");
    assert_eq!(row, stored, "after 2: row content unchanged");
    signed_over(&case, B_OUT, c1, &first, "after 2: C1 over fh1").await;
    unchanged(&case, B_OUT, c1, &in_c1, "after 2: C1 unchanged").await;
    case.zero_drift("step 2").await;

    let by_client = case
        .commit_nar(NarCommit {
            file_hash: "fh2".into(),
            ..commit_of_b(SignTargets::Cache(c2))
        })
        .await;

    assert_eq!(by_client.signed, vec![], "after 3: a client signs nothing");
    unchanged(&case, B_OUT, c2, &in_c2, "after 3: C2 as after step 1").await;
    let row = case.cached_path(B_OUT).await;
    assert_eq!(row, stored, "after 3: row unchanged");
    case.zero_drift("step 3").await;
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
async fn k07_a_committed_path_is_claimed_for_every_project_waiting_on_it() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let (c1, c2) = (CacheId::now_v7(), CacheId::now_v7());
    let case = Case::start(
        chain(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .evaluation(e2)
            .into_cache(c2)
            .keyed(Case::key())
            .derivation(B)
            .job(),
    )
    .await;

    case.commit_nar(commit_of_b(SignTargets::None)).await;

    let in_c1 = signed(&case, B_OUT, c1, "after 1: C1 claimed").await;
    let in_c2 = signed(&case, B_OUT, c2, "after 1: C2 claimed").await;
    case.zero_drift("step 1").await;

    crate::claims::claim_committed_paths(&case.ctx, &[B_OUT.to_owned()])
        .await
        .expect("the claim is written");

    unchanged(&case, B_OUT, c1, &in_c1, "after 2: C1 unchanged").await;
    unchanged(&case, B_OUT, c2, &in_c2, "after 2: C2 unchanged").await;
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
        targets: case.project_caches(e1).await,
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
    let stored = commit_of_b(case.project_caches(e1).await);
    case.commit_nar(stored.clone()).await;

    let mapper = case.ctx.store_path_mapper(MappingContext::Cache(c1));
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
