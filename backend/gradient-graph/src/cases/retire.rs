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
    A, A_OUT, B, B_OUT, B_OUT2, C, C_OUT, Case, D, D_OUT, F_OUT, F1, F2, P, P_OUT, UNKNOWN_OUT, X,
    X_OUT, as_set, chain, chain_done, commit_of, drv, drv_nar, pass, with_output,
};
use crate::messages::{Demotion, NarCommit};

fn drv_file_hash(drv: &str) -> &str {
    let (hash, _) = drv.split_once('-').expect("a drv path");
    hash
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
    let (e1, c1) = (EvaluationId::now_v7(), CacheId::now_v7());
    let case = Case::start(chain_done(e1).into_cache(c1).keyed(Case::key())).await;
    case.seed_signature(B_OUT, c1, Some(vec![7; 64])).await;
    let start = state_of_the_chain(&case, e1).await;

    let report = case
        .report_missing_nar_of_every_content_variant(B_OUT)
        .await;

    let stored = case.find_cached_path(B_OUT).await;
    assert_eq!(stored, None, "after 1: cached_path(B_OUT)");
    let signatures = case.db.rows::<cached_path_signature::Entity>().await;
    let none = signatures.is_empty();
    assert!(none, "after 1: {signatures:?}");
    let b = case.sb(B).await;
    let queue = (b.status, b.wanted, b.cache_available);
    assert_eq!(queue, (Queued, true, false), "after 1: sb(B)");
    let closure = (b.fetchable, b.missing_runtime_deps);
    assert_eq!(closure, (false, 0), "after 1: sb(B)");
    let a = case.queue_state(A).await;
    assert_eq!(a, (Created, 1), "after 1: sb(A)");
    let producers = as_set(&report.producers);
    let only_b = case.ids(&[B]).await;
    assert_eq!(producers, only_b, "after 1: report.producers");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 2, 0, 1, 0], "after 1: counters");
    case.zero_drift("step 1").await;

    let commit = NarCommit {
        producer: case.make_worker_producer_for_evaluation_project(e1).await,
        ..commit_of(B_OUT, "b", &[(B_OUT, "b")])
    };

    rebuild(&case, B, commit).await;

    let b = case.sb(B).await;
    let b = (b.status, b.fetchable);
    assert_eq!(b, (Completed, true), "after 2: sb(B)");
    let a = case.queue_state(A).await;
    assert_eq!(a, (Queued, 0), "after 2: sb(A)");
    let signature = case.signature(B_OUT, c1).await;
    let signed = signature.is_some_and(|row| row.signature.is_some());
    assert!(signed, "after 2: signature(B_OUT, C1)");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 1, 0, 1, 0], "after 2: counters");
    let end = state_of_the_chain(&case, e1).await;
    assert_eq!(end, start, "after 2: the CHAIN_DONE start");
    case.zero_drift("step 2").await;
    case.finish().await;
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

    case.report_missing_nar_of_every_content_variant(D_OUT)
        .await;

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
async fn r03_removing_cached_drv_file_marks_derivation_not_walked_moves_queued_build_to_created_reupload_alone_keeps_it_created_and_new_walk_queues_it_again()
 {
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

    case.report_missing_nar_of_every_content_variant(drv_file_hash(B))
        .await;
    case.report_missing_nar_of_every_content_variant(drv_file_hash(C))
        .await;

    let b = case.sb(B).await;
    let b = (b.status, b.wanted);
    assert_eq!(b, (Created, true), "after 1: sb(B)");
    let walked = case.derivation(B).await.walked;
    assert!(!walked, "after 1: derivation(B).walked");
    let c = case.sb(C).await.status;
    assert_eq!(c, Building, "after 1: sb(C).status");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [3, 3, 0, 0, 1], "after 1: counters(E1)");
    case.zero_drift("step 1").await;

    case.commit_nar(drv_nar(B)).await;

    let b = case.sb(B).await.status;
    assert_eq!(b, Created, "after 2: sb(B).status");
    let walked = case.derivation(B).await.walked;
    assert!(!walked, "after 2: derivation(B).walked");
    case.zero_drift("step 2").await;

    let leaf = with_output(drv(B, &[]), B_OUT);
    case.walk(case.tasked(e1, vec![leaf]).await).await;

    let b = case.sb(B).await.status;
    assert_eq!(b, Queued, "after 3: sb(B).status");
    case.zero_drift("step 3").await;
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
    let frozen = case.counters(e1).await;

    case.report_missing_nar_of_every_content_variant(A_OUT)
        .await;

    let a = case.queue_state(A).await;
    assert_eq!(a, (Created, 1), "after 1: sb(A) status, blocking_deps");
    let a = case.sb(A).await;
    let a = (a.cache_available, a.probed, a.wanted);
    assert_eq!(a, (false, true, true), "after 1: sb(A)");
    let b = case.sb(B).await;
    let b = (b.status, b.attempt, b.wanted);
    assert_eq!(b, (Queued, 0, true), "after 1: sb(B)");
    let counters = case.counters(e1).await;
    assert_eq!(
        counters, frozen,
        "after 1: counters(E1), frozen at the verdict"
    );
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

    let report = case
        .report_missing_nar_of_every_content_variant(F_OUT)
        .await;

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

    case.report_missing_nar_of_every_content_variant(B_OUT)
        .await;

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

    let variants = case.content_variant_ids(B_OUT).await;
    let report = case.report_missing_nar_of_content_variants(&variants).await;
    let producers = as_set(&report.producers);
    let only_b = case.ids(&[B]).await;
    assert_eq!(producers, only_b, "after 1: producers");

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

    let repeated = case.report_missing_nar_of_content_variants(&variants).await;
    assert!(repeated.producers.is_empty(), "after 2: producers");

    let rows = case.rows().await;
    assert_eq!(rows, after_first, "after 2: identical to after 1");
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

    case.report_missing_nar_of_every_content_variant(B_OUT)
        .await;

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

#[tokio::test]
async fn r10_demoting_a_stored_unneeded_output_of_a_complete_shared_build_counts_one_missing_runtime_dependency()
 {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        shape()
            .evaluation(e1)
            .derivation(D)
            .walked()
            .entry_point("pkg.d")
            .job()
            .probed()
            .drv_cached()
            .output(D_OUT)
            .cached_output(D)
            .build_status(Completed)
            .runtime_depends_on(B)
            .derivation(B)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .cached_output(B)
            .unneeded_output(B_OUT2, "dev")
            .cached_path(B_OUT2, "b-dev")
            .build_status(Completed),
    )
    .await;
    let d = case.closure_state(D).await;
    assert_eq!(d, (true, 0), "after the seed: sb(D) fetchable, missing");

    case.report_missing_nar_of_every_content_variant(B_OUT2)
        .await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.fetchable, b.unneeded_outputs),
        (false, Vec::<String>::new()),
        "after 1: sb(B) fetchable, unneeded_outputs"
    );
    let d = case.closure_state(D).await;
    assert_eq!(d, (false, 1), "after 1: sb(D) fetchable, missing");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn r10_a_cache_claim_demotes_only_the_content_signed_in_that_cache() {
    use gradient_entity::cached_path::{Column as CCachedPath, Entity as ECachedPath};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    let (e1, c1, c2) = (EvaluationId::now_v7(), CacheId::now_v7(), CacheId::now_v7());
    let case = Case::start(
        chain_done(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .into_cache(c2)
            .keyed(Case::key()),
    )
    .await;
    let other_key = format!("{B_OUT}-0therc0ntentfi1ehash");
    case.seed_signature(B_OUT, c1, Some(vec![7; 64])).await;
    case.sql(&format!(
        "INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, file_size, \
             nar_size, nar_hash, debug_info_indexed, server_computed_hashes_and_sizes, \
             created_at) \
         SELECT uuidv7(), hash, '{other_key}', package, 'sha256:other', file_size, nar_size, \
             'sha256:othernar', false, true, created_at \
         FROM cached_path WHERE hash = '{B_OUT}'; \
         INSERT INTO cached_path_signature (id, cached_path, cache, signature, fetch_count, created_at) \
         SELECT uuidv7(), id, '{c2}', decode(repeat('08', 64), 'hex'), 0, now() \
         FROM cached_path WHERE nar_object_storage_key = '{other_key}';"
    ))
    .await;
    case.store_object(B_OUT).await;
    case.store_object(&other_key).await;

    case.demote(Demotion::CacheClaim {
        cache: c1,
        hash: B_OUT.to_owned(),
    })
    .await;

    let claims = cached_path_signature::Entity::find()
        .filter(cached_path_signature::Column::Cache.eq(c1))
        .all(&case.db.connection())
        .await
        .expect("the claims are read");
    assert!(claims.is_empty(), "The claims of cache C1 are missing.");
    let kept: Vec<String> = ECachedPath::find()
        .filter(CCachedPath::Hash.eq(B_OUT))
        .all(&case.db.connection())
        .await
        .expect("the stored contents are read")
        .into_iter()
        .map(|row| row.nar_object_storage_key)
        .collect();
    assert_eq!(
        kept,
        vec![other_key.clone()],
        "Contents of other projects remain stored with their C2 claims."
    );
    assert!(
        case.signature(B_OUT, c2).await.is_some(),
        "Contents of other projects remain stored with their C2 claims."
    );
    let object_kept = case.ctx.storage.nar_storage.exists(&other_key).await;
    assert!(
        object_kept.expect("the storage is probed"),
        "The NAR object for the other content is still in storage."
    );
    assert!(
        case.sb(B).await.fetchable,
        "The producer can remain fetchable while other contents of its output stay in storage."
    );
    case.finish().await;
}
