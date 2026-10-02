/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! An output is probed once its shared build is needed, never on its batch landing. Probing is HTTP
//! and must stay off every graph path. A network round trip would otherwise hold the single graph
//! writer's transaction.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use gradient_core::ServerState;
use gradient_entity::StorePath;
use gradient_types::*;
use gradient_util::supervision::ChildSpec;
use sea_orm::{ColumnTrait, EntityTrait, JoinType, QueryFilter, QuerySelect, RelationTrait};
use tokio::sync::Mutex;
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::debug;

pub const HEALTH_NAME: &str = "upstream-probe";
const PROBE_TICK: Duration = Duration::from_secs(1);
const PROBE_BUDGET: Duration = Duration::from_secs(120);
const PROBE_MEMORY: Duration = Duration::from_secs(300);
const PROBE_BATCH: usize = 256;

const PROBE_SWEEP: Duration = Duration::from_secs(60);
const PROBE_DESCENT: Duration = Duration::from_secs(60);

pub fn child_spec(state: &Arc<ServerState>) -> ChildSpec {
    let inbox = Arc::new(Mutex::new(state.probe_requests.take_inbox()));
    let seen: Arc<Mutex<HashMap<DerivationId, Instant>>> = Arc::new(Mutex::new(HashMap::new()));
    let swept = Arc::new(Mutex::new(Instant::now()));
    let state = Arc::clone(state);
    ChildSpec::periodic(HEALTH_NAME, PROBE_TICK, PROBE_BUDGET, move || {
        let state = Arc::clone(&state);
        let inbox = Arc::clone(&inbox);
        let seen = Arc::clone(&seen);
        let swept = Arc::clone(&swept);
        async move {
            probe_pass(&state, &inbox, &seen, &swept)
                .await
                .map_err(|e| gradient_util::supervision::PassError::from(e.to_string()))
        }
    })
}

async fn probe_pass(
    state: &Arc<ServerState>,
    inbox: &Mutex<Option<UnboundedReceiver<Vec<DerivationId>>>>,
    seen: &Mutex<HashMap<DerivationId, Instant>>,
    swept: &Mutex<Instant>,
) -> Result<()> {
    let started = Instant::now();
    let mut requested = drain_requests(inbox).await;
    if requested.is_empty() && sweep_due(swept).await {
        requested.extend(unanswered_need(state).await?);
    }

    while !requested.is_empty() {
        let unanswered = probe_round(state, fresh(requested, seen).await).await?;
        forget(seen, &unanswered).await;
        if started.elapsed() > PROBE_DESCENT {
            break;
        }

        requested = drain_requests(inbox).await;
    }

    Ok(())
}

async fn probe_round(
    state: &Arc<ServerState>,
    shared_builds: Vec<DerivationId>,
) -> Result<Vec<DerivationId>> {
    if shared_builds.is_empty() {
        return Ok(Vec::new());
    }

    let plan = plan_probes(state, &shared_builds).await?;
    for (evaluation, targets) in plan.rounds {
        for chunk in targets.chunks(PROBE_BATCH) {
            let hits = crate::eval::probe_outputs(state, &evaluation, chunk.to_vec()).await;
            debug!(
                hits = hits.len(),
                asked = chunk.len(),
                "upstream probe round"
            );
            if hits.is_empty() {
                continue;
            }
            state
                .graph
                .upstream_hits(hits)
                .await
                .context("apply what the upstream probe found")?;
        }
    }

    state
        .graph
        .upstream_probed(plan.answered.clone())
        .await
        .context("record the shared builds this round answered for")?;

    let answered: HashSet<DerivationId> = plan.answered.into_iter().collect();
    Ok(shared_builds
        .into_iter()
        .filter(|a| !answered.contains(a))
        .collect())
}

async fn drain_requests(
    inbox: &Mutex<Option<UnboundedReceiver<Vec<DerivationId>>>>,
) -> HashSet<DerivationId> {
    let mut requested: HashSet<DerivationId> = HashSet::new();
    if let Some(rx) = inbox.lock().await.as_mut() {
        while let Ok(batch) = rx.try_recv() {
            requested.extend(batch);
        }
    }

    requested
}

async fn sweep_due(swept: &Mutex<Instant>) -> bool {
    let mut last = swept.lock().await;
    if last.elapsed() < PROBE_SWEEP {
        return false;
    }
    *last = Instant::now();

    true
}

/// The request channel is in memory. A process stopping between the commit and the send is leaving
/// shared builds nothing will ever ask about. The need is stopping at an unanswered one, and
/// nothing below it would ever build again.
async fn unanswered_need(state: &Arc<ServerState>) -> Result<Vec<DerivationId>> {
    Ok(unanswered_need_query()
        .all(&state.worker_db)
        .await
        .context("find needed builds the upstream probe was never asked about")?
        .into_iter()
        .map(|a| a.derivation)
        .collect())
}

fn unanswered_need_query() -> sea_orm::Select<EDerivationBuild> {
    EDerivationBuild::find()
        .join(
            JoinType::InnerJoin,
            gradient_entity::derivation_build::Relation::Derivation.def(),
        )
        .filter(CDerivationBuild::Probed.eq(false))
        .filter(CDerivationBuild::Wanted.eq(true))
        .filter(CDerivation::Walked.eq(true))
        .limit(PROBE_BATCH as u64)
}

async fn fresh(
    requested: HashSet<DerivationId>,
    seen: &Mutex<HashMap<DerivationId, Instant>>,
) -> Vec<DerivationId> {
    if requested.is_empty() {
        return Vec::new();
    }

    let now = Instant::now();
    let mut seen = seen.lock().await;
    seen.retain(|_, at| now.duration_since(*at) < PROBE_MEMORY);

    let mut fresh: Vec<DerivationId> = requested
        .into_iter()
        .filter(|d| !seen.contains_key(d))
        .collect();
    fresh.sort_unstable();
    for d in &fresh {
        seen.insert(*d, now);
    }

    fresh
}

async fn forget(seen: &Mutex<HashMap<DerivationId, Instant>>, unanswered: &[DerivationId]) {
    if unanswered.is_empty() {
        return;
    }

    let mut seen = seen.lock().await;
    for d in unanswered {
        seen.remove(d);
    }
}

#[derive(Debug, Default)]
pub(crate) struct ProbePlan {
    pub rounds: Vec<(MEvaluation, Vec<(String, String)>)>,
    pub answered: Vec<DerivationId>,
}

pub(crate) async fn plan_probes(
    state: &Arc<ServerState>,
    shared_builds: &[DerivationId],
) -> Result<ProbePlan> {
    let db = &state.worker_db;
    let wanted: Vec<DerivationId> =
        gradient_db::fetch_in_chunks(shared_builds, |chunk| async move {
            EDerivationBuild::find()
                .filter(CDerivationBuild::Derivation.is_in(chunk))
                .filter(CDerivationBuild::Wanted.eq(true))
                .all(db)
                .await
        })
        .await
        .context("load which requested shared builds are wanted")?
        .into_iter()
        .map(|a| a.derivation)
        .collect();
    if wanted.is_empty() {
        return Ok(ProbePlan::default());
    }

    let outputs = gradient_db::fetch_in_chunks(&wanted, |chunk| async move {
        EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.is_in(chunk))
            .all(db)
            .await
    })
    .await
    .context("load the outputs of the shared builds that became wanted")?;
    let recorded: HashSet<DerivationId> = outputs.iter().map(|o| o.derivation).collect();
    let answered: Vec<DerivationId> = wanted
        .into_iter()
        .filter(|d| recorded.contains(d))
        .collect();

    let jobs = gradient_db::fetch_in_chunks(&answered, |chunk| async move {
        EBuildJob::find()
            .filter(CBuildJob::Derivation.is_in(chunk))
            .all(db)
            .await
    })
    .await
    .context("find an evaluation naming each shared build that became wanted")?;
    let mut evaluation_of: HashMap<DerivationId, EvaluationId> = HashMap::new();
    for job in &jobs {
        evaluation_of
            .entry(job.derivation)
            .or_insert(job.evaluation);
    }

    let mut by_evaluation: HashMap<EvaluationId, HashMap<String, String>> = HashMap::new();
    for o in outputs.iter().filter(|o| !o.is_cached_anywhere()) {
        let Some(evaluation) = evaluation_of.get(&o.derivation) else {
            continue;
        };
        let sp = StorePath::from_parts(o.hash.clone(), o.package.clone());
        by_evaluation
            .entry(*evaluation)
            .or_default()
            .insert(o.hash.clone(), sp.full());
    }
    if by_evaluation.is_empty() {
        return Ok(ProbePlan {
            rounds: Vec::new(),
            answered,
        });
    }

    let ids: Vec<EvaluationId> = by_evaluation.keys().copied().collect();
    let evaluations = gradient_db::fetch_in_chunks(&ids, |chunk| async move {
        EEvaluation::find()
            .filter(CEvaluation::Id.is_in(chunk))
            .all(db)
            .await
    })
    .await
    .context("load the evaluations the probe reads its upstream caches through")?;

    let rounds = evaluations
        .into_iter()
        .filter_map(|e| {
            by_evaluation
                .remove(&e.id)
                .map(|targets| (e, targets.into_iter().collect()))
        })
        .collect();

    Ok(ProbePlan { rounds, answered })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::build_job::Model as MBuildJob;
    use gradient_entity::derivation_output::Model as MDerivationOutput;
    use gradient_test_support::prelude::test_state;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn output(derivation: DerivationId, hash: &str, cached: bool) -> MDerivationOutput {
        MDerivationOutput {
            id: gradient_types::ids::DerivationOutputId::now_v7(),
            derivation,
            name: "out".to_owned(),
            hash: hash.to_owned(),
            package: "hello-2.12".to_owned(),
            is_cached: cached,
            ..Default::default()
        }
    }

    fn wanted(derivation: DerivationId) -> MDerivationBuild {
        MDerivationBuild {
            id: gradient_types::ids::DerivationBuildId::now_v7(),
            derivation,
            wanted: true,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn already_cached_outputs_are_not_probed() {
        let derivation = DerivationId::now_v7();
        let evaluation = EvaluationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![wanted(derivation)]])
            .append_query_results([vec![
                output(derivation, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", false),
                output(derivation, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", true),
            ]])
            .append_query_results([vec![MBuildJob {
                id: gradient_types::ids::BuildJobId::now_v7(),
                evaluation,
                derivation,
                ..Default::default()
            }]])
            .append_query_results([vec![MEvaluation {
                id: evaluation,
                ..Default::default()
            }]])
            .into_connection();

        let planned = plan_probes(&test_state(db), &[derivation])
            .await
            .expect("the plan is read")
            .rounds;

        assert_eq!(planned.len(), 1, "{planned:?}");
        let (_, targets) = &planned[0];
        assert_eq!(
            targets.iter().map(|(h, _)| h.as_str()).collect::<Vec<_>>(),
            vec!["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
            "an output already in our cache must not be asked for"
        );
    }

    #[tokio::test]
    async fn the_sweep_runs_at_most_once_an_interval() {
        let swept = Mutex::new(Instant::now());
        assert!(!sweep_due(&swept).await, "a fresh mark is not due");
    }

    #[tokio::test]
    async fn an_idle_pass_reads_nothing() {
        let state = test_state(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

        probe_pass(
            &state,
            &Mutex::new(None),
            &Mutex::new(HashMap::new()),
            &Mutex::new(Instant::now()),
        )
        .await
        .expect("an idle tick is a no-op");
    }

    #[tokio::test]
    async fn an_unnamed_shared_build_is_not_probed() {
        let derivation = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![wanted(derivation)]])
            .append_query_results([vec![output(
                derivation,
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                false,
            )]])
            .append_query_results([Vec::<MBuildJob>::new()])
            .into_connection();

        assert!(
            plan_probes(&test_state(db), &[derivation])
                .await
                .expect("the plan is read")
                .rounds
                .is_empty()
        );
    }

    #[tokio::test]
    async fn an_unwalked_shared_build_is_left_unanswered() {
        let walked = DerivationId::now_v7();
        let stub = DerivationId::now_v7();
        let evaluation = EvaluationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![wanted(walked), wanted(stub)]])
            .append_query_results([vec![output(
                walked,
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                false,
            )]])
            .append_query_results([vec![MBuildJob {
                id: gradient_types::ids::BuildJobId::now_v7(),
                evaluation,
                derivation: walked,
                ..Default::default()
            }]])
            .append_query_results([vec![MEvaluation {
                id: evaluation,
                ..Default::default()
            }]])
            .into_connection();

        let plan = plan_probes(&test_state(db), &[walked, stub])
            .await
            .expect("the plan is read");

        assert_eq!(plan.answered, vec![walked]);
    }

    #[tokio::test]
    async fn an_unwanted_shared_build_is_left_unanswered() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MDerivationBuild>::new()])
            .into_connection();

        let plan = plan_probes(&test_state(db), &[DerivationId::now_v7()])
            .await
            .expect("the plan is read");

        assert!(
            plan.rounds.is_empty() && plan.answered.is_empty(),
            "{plan:?}"
        );
    }

    #[tokio::test]
    async fn an_unanswered_shared_build_is_asked_again() {
        let seen = Mutex::new(HashMap::new());
        let shared_build = DerivationId::now_v7();
        assert_eq!(
            fresh(HashSet::from([shared_build]), &seen).await,
            vec![shared_build]
        );

        forget(&seen, &[shared_build]).await;

        assert_eq!(
            fresh(HashSet::from([shared_build]), &seen).await,
            vec![shared_build]
        );
    }

    #[test]
    fn the_sweep_reads_only_walked_shared_builds() {
        use sea_orm::QueryTrait as _;
        let sql = unanswered_need_query()
            .build(DatabaseBackend::Postgres)
            .to_string();
        assert!(sql.contains(r#""derivation"."walked" = TRUE"#), "{sql}");
    }
}
