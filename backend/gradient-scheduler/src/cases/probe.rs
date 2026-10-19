/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use gradient_entity::UnresolvableProjectMapping;
use gradient_entity::build::BuildStatus;
use gradient_entity::cache_upstream::CacheUpstreamKind;
use gradient_entity::derivation_build;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::evaluation_message::MessageLevel;
use gradient_entity::ids::{CacheId, CacheUpstreamId, DerivationId, EvaluationId};
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_fixtures::shape::{Shape, shape};

use super::{
    A, A_DEV, A_OUT, A1, A1_OUT, A2, A2_OUT, A3, A3_OUT, B, B_OUT, C, C_OUT, D, D_OUT, N, N_OUT, S,
    SchedulerCase, UpstreamServer, UpstreamTally, attr_of, planned_targets, w_build,
};

const W1: &str = "w1";
const PROBE_SOURCE: &str = "upstream-probe";

#[tokio::test]
async fn p01_planning_keeps_wanted_unprobed_builds_and_drops_stubs_and_cached_outputs() {
    let e1 = EvaluationId::now_v7();
    let u1 = UpstreamServer::start().await;
    let given = shape()
        .evaluation(e1)
        .derivation(A)
        .walked()
        .entry_point(&attr_of(A))
        .job()
        .output(A_OUT)
        .depends_on(B)
        .depends_on(S)
        .derivation(B)
        .walked()
        .job()
        .output(B_OUT)
        .cached_output(B)
        .build_status(BuildStatus::Skipped)
        .derivation(S)
        .build_status(BuildStatus::Skipped)
        .derivation(N)
        .walked()
        .job()
        .output(N_OUT)
        .build_status(BuildStatus::Skipped)
        .into_cache(CacheId::now_v7())
        .http_upstream(&u1, CacheSubscriptionMode::ReadWrite, true);
    let case = SchedulerCase::start(given).await;
    let (b, s, n) = (
        case.db.id_of(B).await,
        case.db.id_of(S).await,
        case.db.id_of(N).await,
    );

    let plan = case.plan_probes(&[A, B, S, N]).await;
    assert_eq!(
        planned_targets(&plan),
        targets(&[(e1, &[A_OUT])]),
        "after 1: result.targets"
    );
    assert!(plan.asked.contains(&b), "after 1: result.asked contains B");
    for (path, id) in [(S, s), (N, n)] {
        assert!(
            !plan.asked.contains(&id) && !plan.owners.values().flatten().any(|o| *o == id),
            "after 1: {path} in no set"
        );
    }
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn p02_dependencies_join_the_round_unless_answered_or_cache_available() {
    let e1 = EvaluationId::now_v7();
    let u1 = UpstreamServer::start().await;
    let given = shape()
        .evaluation(e1)
        .derivation(A)
        .walked()
        .entry_point(&attr_of(A))
        .job()
        .output(A_OUT)
        .depends_on(B)
        .depends_on(C)
        .depends_on(D)
        .derivation(B)
        .walked()
        .job()
        .output(B_OUT)
        .build_status(BuildStatus::Skipped)
        .derivation(C)
        .walked()
        .job()
        .output(C_OUT)
        .probed()
        .build_status(BuildStatus::Skipped)
        .derivation(D)
        .walked()
        .job()
        .output(D_OUT)
        .probed()
        .cache_available()
        .build_status(BuildStatus::Skipped)
        .into_cache(CacheId::now_v7())
        .http_upstream(&u1, CacheSubscriptionMode::ReadWrite, true);
    let case = SchedulerCase::start(given).await;

    let plan = case.plan_probes(&[A]).await;
    assert_eq!(
        planned_targets(&plan),
        targets(&[(e1, &[A_OUT, B_OUT])]),
        "after 1: result.targets"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn p03_a_signed_hit_on_every_output_makes_the_build_a_passthrough() {
    let e1 = EvaluationId::now_v7();
    let u1 = UpstreamServer::start().await;
    u1.hit(A_OUT, "a", &[]).await;
    let given = shape()
        .evaluation(e1)
        .evaluation_status(EvaluationStatus::Building)
        .derivation(A)
        .walked()
        .entry_point(&attr_of(A))
        .job()
        .output(A_OUT)
        .build_status(BuildStatus::Created)
        .into_cache(CacheId::now_v7())
        .http_upstream(&u1, CacheSubscriptionMode::ReadWrite, true);
    let case = SchedulerCase::start(given).await;

    case.probe_round(&[A]).await;
    let output = case.output_of(A, "out").await;
    assert_eq!(
        (
            output.external_url.is_some(),
            output.nar_hash.is_some(),
            output.file_hash.is_some(),
            output.file_size.is_some(),
            output.references.as_deref(),
        ),
        (true, true, true, true, Some("")),
        "after 1: output(A, out)"
    );
    assert_eq!(
        build_state(&case.sb(A).await),
        (true, true, BuildStatus::Queued),
        "after 1: sb(A) {{probed, cache_available, status}}"
    );
    assert_eq!(
        case.upstream_metrics().await,
        vec![tally(&u1, 1, 1, 0)],
        "after 1: upstream_metric(U1.url)"
    );
    case.check("step 1").await;

    case.admit_startable_moves().await;
    let key = case.key(A).await;
    assert_eq!(
        case.pending_build_job(&key).await.map(|j| j.substitute),
        Some(true),
        "after 2: pending(build:A) as Substitute"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn p04_a_404_from_every_upstream_is_a_final_miss() {
    let e1 = EvaluationId::now_v7();
    let (u1, u2) = (UpstreamServer::start().await, UpstreamServer::start().await);
    for upstream in [&u1, &u2] {
        upstream.miss(A_OUT).await;
        upstream.miss(B_OUT).await;
    }
    let given = unprobed_chain(e1)
        .into_cache(CacheId::now_v7())
        .http_upstream(&u1, CacheSubscriptionMode::ReadWrite, true)
        .http_upstream(&u2, CacheSubscriptionMode::ReadWrite, true);
    let case = SchedulerCase::start(given).await;
    let (a, b) = (case.db.id_of(A).await, case.db.id_of(B).await);

    let plan = case.plan_probes(&[A]).await;
    assert_eq!(
        plan.asked
            .iter()
            .copied()
            .collect::<BTreeSet<DerivationId>>(),
        BTreeSet::from([a, b]),
        "after 1: plan.asked"
    );
    case.probe_round(&[A]).await;
    let shared_a = case.sb(A).await;
    assert_eq!(
        (build_state(&shared_a), shared_a.blocking_deps),
        ((true, false, BuildStatus::Created), 1),
        "after 1: sb(A)"
    );
    let shared_b = case.sb(B).await;
    assert_eq!(
        (shared_b.wanted, shared_b.probed, shared_b.status),
        (true, true, BuildStatus::Queued),
        "after 1: sb(B)"
    );
    assert_eq!(
        case.upstream_metrics().await,
        sorted_tallies(vec![tally(&u1, 2, 0, 2), tally(&u2, 2, 0, 2)]),
        "after 1: upstream_metric of U1 and U2"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn p05_a_silent_upstream_keeps_the_build_unanswered_and_warns_once() {
    let e1 = EvaluationId::now_v7();
    let (u1, u2) = (UpstreamServer::start().await, UpstreamServer::start().await);
    u1.miss(A_OUT).await;
    u1.miss(B_OUT).await;
    u2.silent(A_OUT).await;
    u2.silent(B_OUT).await;
    let given = unprobed_chain(e1)
        .into_cache(CacheId::now_v7())
        .http_upstream(&u1, CacheSubscriptionMode::ReadWrite, true)
        .http_upstream(&u2, CacheSubscriptionMode::ReadWrite, true);
    let case = SchedulerCase::start(given).await;

    case.probe_round(&[A]).await;
    let shared = case.sb(A).await;
    assert_eq!(
        (shared.probed, shared.cache_available),
        (false, false),
        "after 1: sb(A)"
    );
    let messages = case.messages(e1).await;
    assert!(
        matches!(messages.as_slice(), [m] if m.level == MessageLevel::Warning
            && m.source.as_deref() == Some(PROBE_SOURCE)
            && m.message.contains(&u2.url())),
        "after 1: evaluation_message(E1) == [{{Warning, upstream-probe, U2.url}}], got {messages:?}"
    );
    case.check("step 1").await;

    case.probe_round(&[A]).await;
    assert!(!case.sb(A).await.probed, "after 2: sb(A).probed");
    assert_eq!(
        case.messages(e1).await.len(),
        1,
        "after 2: evaluation_message(E1) still a single row"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn p06_an_error_is_no_answer_and_an_unsigned_or_wrongly_signed_narinfo_is_a_miss() {
    let e1 = EvaluationId::now_v7();
    let u1 = UpstreamServer::start().await;
    u1.error(A1_OUT).await;
    u1.unsigned(A2_OUT, "a2").await;
    u1.wrongly_signed(A3_OUT, "a3").await;
    let given = shape()
        .evaluation(e1)
        .evaluation_status(EvaluationStatus::Building)
        .unprobed(A1, A1_OUT)
        .unprobed(A2, A2_OUT)
        .unprobed(A3, A3_OUT)
        .into_cache(CacheId::now_v7())
        .http_upstream(&u1, CacheSubscriptionMode::ReadWrite, true);
    let case = SchedulerCase::start(given).await;

    case.probe_round(&[A1, A2, A3]).await;
    assert!(!case.sb(A1).await.probed, "after 1: sb(A1).probed");
    for path in [A2, A3] {
        let shared = case.sb(path).await;
        assert_eq!(
            (shared.probed, shared.cache_available),
            (true, false),
            "after 1: sb({path})"
        );
    }
    assert_eq!(
        case.output_of(A3, "out").await.external_url,
        None,
        "after 1: output(A3, out).external_url"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn p07_a_project_without_a_candidate_upstream_gets_a_final_miss_at_once() {
    let e1 = EvaluationId::now_v7();
    let given = shape()
        .evaluation(e1)
        .evaluation_status(EvaluationStatus::Building)
        .unprobed(A, A_OUT);
    let case = SchedulerCase::start(given).await;

    case.plan_probes(&[A]).await;
    case.probe_round(&[A]).await;
    assert_eq!(
        build_state(&case.sb(A).await),
        (true, false, BuildStatus::Queued),
        "after 1: sb(A) {{probed, cache_available, status}}"
    );
    assert_eq!(
        case.upstream_metrics().await,
        vec![],
        "after 1: no upstream_metric rows"
    );
    assert_eq!(
        case.messages(e1).await,
        vec![],
        "after 1: no evaluation_message"
    );
    case.check("step 1").await;

    case.connect(W1, w_build(), &[]).await;
    case.refresh_waiting_state().await;
    let evaluation = case.evaluation(e1).await;
    assert_eq!(
        (evaluation.status, evaluation.waiting_reason),
        (EvaluationStatus::Building, None),
        "after 2: evaluation(E1)"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn p09_write_only_inactive_and_non_http_upstreams_are_not_asked() {
    let e1 = EvaluationId::now_v7();
    let (u1, u2, u3, u4) = (
        UpstreamServer::start().await,
        UpstreamServer::start().await,
        UpstreamServer::start().await,
        UpstreamServer::start().await,
    );
    for upstream in [&u1, &u2, &u3, &u4] {
        upstream.miss(A_OUT).await;
    }
    let given = shape()
        .evaluation(e1)
        .evaluation_status(EvaluationStatus::Building)
        .unprobed(A, A_OUT)
        .into_cache(CacheId::now_v7())
        .http_upstream(&u1, CacheSubscriptionMode::WriteOnly, true)
        .http_upstream(&u2, CacheSubscriptionMode::ReadWrite, false)
        .upstream(
            CacheUpstreamId::now_v7(),
            &u3.url(),
            &u3.public_key,
            CacheUpstreamKind::GradientProto,
            CacheSubscriptionMode::ReadWrite,
            true,
        )
        .http_upstream(&u4, CacheSubscriptionMode::ReadOnly, true);
    let case = SchedulerCase::start(given).await;

    case.probe_round(&[A]).await;
    let asked: Vec<String> = case
        .upstream_metrics()
        .await
        .into_iter()
        .map(|t| t.url)
        .collect();
    assert_eq!(
        asked,
        vec![u4.url()],
        "after 1: upstream_metric rows only for U4"
    );
    assert!(case.sb(A).await.probed, "after 1: sb(A).probed");
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn p10_the_sweep_asks_every_walked_unprobed_builder_and_leaves_stubs_alone() {
    let e1 = EvaluationId::now_v7();
    let u1 = UpstreamServer::start().await;
    u1.hit(A_OUT, "a", &[]).await;
    let given = shape()
        .evaluation(e1)
        .evaluation_status(EvaluationStatus::Building)
        .unprobed(A, A_OUT)
        .runtime_depends_on(S)
        .derivation(S)
        .into_cache(CacheId::now_v7())
        .http_upstream(&u1, CacheSubscriptionMode::ReadWrite, true);
    let case = SchedulerCase::start(given).await;

    case.probe_sweep().await;
    let shared = case.sb(A).await;
    assert_eq!(
        (shared.probed, shared.cache_available),
        (true, true),
        "after 1: sb(A)"
    );
    assert!(!case.sb(S).await.probed, "after 1: sb(S).probed");
    assert_eq!(
        case.upstream_metrics()
            .await
            .into_iter()
            .find(|t| t.url == u1.url())
            .map(|t| t.requests),
        Some(1),
        "after 1: upstream_metric(U1).request_count"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn p11_a_build_named_by_two_projects_is_asked_once() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let (u1, u2) = (UpstreamServer::start().await, UpstreamServer::start().await);
    u1.hit(A_OUT, "a", &[]).await;
    u2.miss(A_OUT).await;
    let given = shape()
        .evaluation(e1)
        .evaluation_status(EvaluationStatus::Building)
        .unprobed(A, A_OUT)
        .into_cache(CacheId::now_v7())
        .http_upstream(&u1, CacheSubscriptionMode::ReadWrite, true)
        .evaluation(e2)
        .evaluation_status(EvaluationStatus::Building)
        .derivation(A)
        .entry_point(&attr_of(A))
        .job()
        .into_cache(CacheId::now_v7())
        .http_upstream(&u2, CacheSubscriptionMode::ReadWrite, true);
    let case = SchedulerCase::start(given).await;

    let planned = planned_targets(&case.plan_probes(&[A]).await);
    let owners: Vec<EvaluationId> = planned
        .iter()
        .filter(|(_, hashes)| hashes.contains(A_OUT))
        .map(|(evaluation, _)| *evaluation)
        .collect();
    assert!(
        matches!(owners.as_slice(), [owner] if *owner == e1 || *owner == e2),
        "after 1: A_OUT under a single evaluation of E1, E2, got {planned:?}"
    );
    case.check("step 1").await;

    case.probe_round(&[A]).await;
    let asked: Vec<String> = case
        .upstream_metrics()
        .await
        .into_iter()
        .map(|t| t.url)
        .collect();
    let shared = case.sb(A).await;
    if asked == vec![u1.url()] {
        assert_eq!(
            build_state(&shared),
            (true, true, BuildStatus::Queued),
            "after 2, asked through E1: sb(A)"
        );
        assert_eq!(
            (case.counters(e1).await, case.counters(e2).await),
            ([1, 1, 0, 1, 0], [1, 1, 0, 1, 0]),
            "after 2, asked through E1: counters(E1), counters(E2)"
        );
    } else {
        assert_eq!(
            asked,
            vec![u2.url()],
            "after 2: the round asks the upstream of a single project"
        );
        assert_eq!(
            (shared.probed, shared.cache_available),
            (true, false),
            "after 2, asked through E2: sb(A)"
        );
    }
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn p12_a_build_with_two_outputs_is_not_probed_while_an_output_has_no_answer() {
    let e1 = EvaluationId::now_v7();
    let u1 = UpstreamServer::start().await;
    u1.hit(A_OUT, "a", &[]).await;
    u1.silent(A_DEV).await;
    let given = shape()
        .evaluation(e1)
        .evaluation_status(EvaluationStatus::Building)
        .derivation(A)
        .walked()
        .drv_cached()
        .entry_point(&attr_of(A))
        .job()
        .named_output(A_OUT, "out")
        .named_output(A_DEV, "dev")
        .build_status(BuildStatus::Created)
        .into_cache(CacheId::now_v7())
        .http_upstream(&u1, CacheSubscriptionMode::ReadWrite, true);
    let case = SchedulerCase::start(given).await;

    case.probe_round(&[A]).await;
    let shared = case.sb(A).await;
    assert_eq!(
        (shared.probed, shared.cache_available),
        (false, false),
        "after 1: sb(A)"
    );
    assert!(
        case.output_of(A, "out").await.external_url.is_some(),
        "after 1: output(A, out).external_url set"
    );
    case.check("step 1").await;

    u1.forget_answers().await;
    u1.hit(A_OUT, "a", &[]).await;
    u1.miss(A_DEV).await;
    case.probe_round(&[A]).await;
    let shared = case.sb(A).await;
    assert_eq!(
        (shared.probed, shared.cache_available),
        (true, false),
        "after 2: sb(A)"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn p13_a_dependency_no_evaluation_names_is_not_asked_and_stays_unprobed() {
    let e1 = EvaluationId::now_v7();
    let u1 = UpstreamServer::start().await;
    u1.miss(A_OUT).await;
    u1.miss(B_OUT).await;
    let given = shape()
        .evaluation(e1)
        .evaluation_status(EvaluationStatus::Building)
        .unprobed(A, A_OUT)
        .depends_on(B)
        .derivation(B)
        .walked()
        .drv_cached()
        .output(B_OUT)
        .build_status(BuildStatus::Skipped)
        .into_cache(CacheId::now_v7())
        .http_upstream(&u1, CacheSubscriptionMode::ReadWrite, true);
    let case = SchedulerCase::start(given).await;
    let (a, b) = (case.db.id_of(A).await, case.db.id_of(B).await);

    let plan = case.plan_probes(&[A]).await;
    assert_eq!(
        (planned_targets(&plan), plan.asked.clone()),
        (targets(&[(e1, &[A_OUT])]), vec![a]),
        "after 1: result.targets and result.asked"
    );
    assert!(
        !plan.owners.values().flatten().any(|o| *o == b),
        "after 1: B owns no asked output"
    );
    case.check("step 1").await;

    case.probe_round(&[A]).await;
    let (shared_a, shared_b) = (case.sb(A).await, case.sb(B).await);
    assert_eq!(
        (
            (shared_a.probed, shared_a.cache_available),
            (shared_b.probed, shared_b.cache_available)
        ),
        ((true, false), (false, false)),
        "after 2: sb(A) is a miss, sb(B) is still unanswered"
    );
    assert_eq!(
        case.upstream_metrics().await,
        vec![tally(&u1, 1, 0, 1)],
        "after 2: upstream_metric(U1.url) counts A_OUT only"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn p14_a_project_whose_store_paths_cannot_be_mapped_is_asked_again_later() {
    let e1 = EvaluationId::now_v7();
    let u1 = UpstreamServer::start().await;
    u1.miss(A_OUT).await;
    let given = shape()
        .evaluation(e1)
        .evaluation_status(EvaluationStatus::Building)
        .unprobed(A, A_OUT)
        .into_cache(CacheId::now_v7())
        .http_upstream(&u1, CacheSubscriptionMode::ReadWrite, true);
    let case = SchedulerCase::start(given).await;

    case.probe_round_mapped(Arc::new(UnresolvableProjectMapping), &[A])
        .await;
    assert_eq!(
        build_state(&case.sb(A).await),
        (false, false, BuildStatus::Created),
        "after 1: sb(A) {{probed, cache_available, status}}"
    );
    assert_eq!(
        case.upstream_metrics().await,
        vec![],
        "after 1: no upstream_metric rows"
    );
    case.check("step 1").await;

    case.probe_round(&[A]).await;
    assert_eq!(
        build_state(&case.sb(A).await),
        (true, false, BuildStatus::Queued),
        "after 2: sb(A) {{probed, cache_available, status}}"
    );
    case.check("step 2").await;

    case.finish().await;
}

trait ProbeShapes {
    fn unprobed(self, path: &str, out: &str) -> Self;
    fn http_upstream(
        self,
        upstream: &UpstreamServer,
        mode: CacheSubscriptionMode,
        active: bool,
    ) -> Self;
}

impl ProbeShapes for Shape {
    fn unprobed(self, path: &str, out: &str) -> Self {
        self.derivation(path)
            .walked()
            .drv_cached()
            .entry_point(&attr_of(path))
            .job()
            .output(out)
            .build_status(BuildStatus::Created)
    }

    fn http_upstream(
        self,
        upstream: &UpstreamServer,
        mode: CacheSubscriptionMode,
        active: bool,
    ) -> Self {
        self.upstream(
            CacheUpstreamId::now_v7(),
            &upstream.url(),
            &upstream.public_key,
            CacheUpstreamKind::Http,
            mode,
            active,
        )
    }
}

fn unprobed_chain(e1: EvaluationId) -> Shape {
    shape()
        .evaluation(e1)
        .evaluation_status(EvaluationStatus::Building)
        .unprobed(A, A_OUT)
        .depends_on(B)
        .derivation(B)
        .walked()
        .drv_cached()
        .job()
        .output(B_OUT)
        .build_status(BuildStatus::Skipped)
}

fn targets(rounds: &[(EvaluationId, &[&str])]) -> BTreeMap<EvaluationId, BTreeSet<String>> {
    rounds
        .iter()
        .map(|(evaluation, hashes)| {
            (
                *evaluation,
                hashes.iter().map(|h| (*h).to_owned()).collect(),
            )
        })
        .collect()
}

fn build_state(shared: &derivation_build::Model) -> (bool, bool, BuildStatus) {
    (shared.probed, shared.cache_available, shared.status)
}

fn tally(upstream: &UpstreamServer, requests: i32, hits: i32, misses: i32) -> UpstreamTally {
    UpstreamTally {
        url: upstream.url(),
        requests,
        hits,
        misses,
    }
}

fn sorted_tallies(mut tallies: Vec<UpstreamTally>) -> Vec<UpstreamTally> {
    tallies.sort_by(|a, b| a.url.cmp(&b.url));
    tallies
}
