/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! An output is probed once its shared build or the shared build above it is needed, never on
//! its batch landing. Probing is HTTP and must stay off every graph path. A network round trip
//! would otherwise hold the single graph writer's transaction.

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
const PROBE_BATCH: usize = 256;

const PROBE_SWEEP: Duration = Duration::from_secs(60);
const PROBE_DESCENT: Duration = Duration::from_secs(60);

pub fn child_spec(state: &Arc<ServerState>) -> ChildSpec {
    let inbox = Arc::new(Mutex::new(state.probe_requests.take_inbox()));
    let swept = Arc::new(Mutex::new(Instant::now()));
    let wake = state.probe_requests.wake();
    let state = Arc::clone(state);
    ChildSpec::periodic_woken(HEALTH_NAME, PROBE_TICK, PROBE_BUDGET, wake, move || {
        let state = Arc::clone(&state);
        let inbox = Arc::clone(&inbox);
        let swept = Arc::clone(&swept);
        async move {
            probe_pass(&state, &inbox, &swept)
                .await
                .map_err(|e| gradient_util::supervision::PassError::from(e.to_string()))
        }
    })
}

async fn probe_pass(
    state: &Arc<ServerState>,
    inbox: &Mutex<Option<UnboundedReceiver<Vec<DerivationId>>>>,
    swept: &Mutex<Instant>,
) -> Result<()> {
    let started = Instant::now();
    let mut requested = drain_requests(inbox).await;
    if requested.is_empty() && sweep_due(swept).await {
        requested.extend(unanswered_need(state).await?);
    }

    while !requested.is_empty() {
        let mut shared_builds: Vec<DerivationId> = requested.into_iter().collect();
        shared_builds.sort_unstable();
        probe_round(state, shared_builds).await?;
        if started.elapsed() > PROBE_DESCENT {
            break;
        }

        requested = drain_requests(inbox).await;
    }

    Ok(())
}

async fn probe_round(state: &Arc<ServerState>, shared_builds: Vec<DerivationId>) -> Result<()> {
    let mut targets = unprobed_wanted(state, &shared_builds).await?;
    targets.extend(unprobed_dependencies(state, &targets).await?);
    let plan = plan_probes(state, &targets).await?;
    let mut unanswered_outputs: HashSet<String> = HashSet::new();
    for (evaluation, targets) in &plan.rounds {
        for chunk in targets.chunks(PROBE_BATCH) {
            let answer = crate::eval::probe_outputs(state, evaluation, chunk.to_vec()).await;
            debug!(
                hits = answer.hits.len(),
                unanswered = answer.unanswered.len(),
                asked = chunk.len(),
                "upstream probe round"
            );
            unanswered_outputs.extend(answer.unanswered);
            if answer.hits.is_empty() {
                continue;
            }
            state
                .graph
                .upstream_hits(answer.hits)
                .await
                .context("apply what the upstream probe found")?;
        }
    }

    state
        .graph
        .upstream_probed(plan.answered_except(&unanswered_outputs))
        .await
        .context("record the shared builds this round answered for")
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

#[derive(Debug, Default)]
pub(crate) struct ProbePlan {
    pub rounds: Vec<(MEvaluation, Vec<(String, String)>)>,
    pub answered: Vec<DerivationId>,
    pub owners: HashMap<String, Vec<DerivationId>>,
}

impl ProbePlan {
    pub(crate) fn answered_except(
        &self,
        unanswered_outputs: &HashSet<String>,
    ) -> Vec<DerivationId> {
        let waiting: HashSet<DerivationId> = unanswered_outputs
            .iter()
            .filter_map(|hash| self.owners.get(hash))
            .flatten()
            .copied()
            .collect();
        self.answered
            .iter()
            .copied()
            .filter(|d| !waiting.contains(d))
            .collect()
    }
}

async fn unprobed_wanted(
    state: &Arc<ServerState>,
    shared_builds: &[DerivationId],
) -> Result<Vec<DerivationId>> {
    let db = &state.worker_db;
    Ok(
        gradient_db::fetch_in_chunks(shared_builds, |chunk| async move {
            EDerivationBuild::find()
                .filter(CDerivationBuild::Derivation.is_in(chunk))
                .filter(CDerivationBuild::Wanted.eq(true))
                .filter(CDerivationBuild::Probed.eq(false))
                .all(db)
                .await
        })
        .await
        .context("load which requested shared builds are wanted")?
        .into_iter()
        .map(|a| a.derivation)
        .collect(),
    )
}

/// A miss makes all dependencies of the shared build needed. Asking for them in the same round
/// saves the probe a round per closure level, and only the dependencies of a hit go unused.
async fn unprobed_dependencies(
    state: &Arc<ServerState>,
    shared_builds: &[DerivationId],
) -> Result<Vec<DerivationId>> {
    let db = &state.worker_db;
    let mut dependencies: Vec<DerivationId> =
        gradient_db::fetch_in_chunks(shared_builds, |chunk| async move {
            EDerivationDependency::find()
                .filter(CDerivationDependency::Derivation.is_in(chunk))
                .all(db)
                .await
        })
        .await
        .context("load the dependencies of the probed shared builds")?
        .into_iter()
        .map(|e| e.dependency)
        .collect();
    dependencies.sort_unstable();
    dependencies.dedup();

    Ok(
        gradient_db::fetch_in_chunks(&dependencies, |chunk| async move {
            EDerivationBuild::find()
                .filter(CDerivationBuild::Derivation.is_in(chunk))
                .filter(CDerivationBuild::Probed.eq(false))
                .filter(CDerivationBuild::CacheAvailable.eq(false))
                .all(db)
                .await
        })
        .await
        .context("load which dependencies the upstream probe has not answered")?
        .into_iter()
        .map(|a| a.derivation)
        .filter(|d| !shared_builds.contains(d))
        .collect(),
    )
}

pub(crate) async fn plan_probes(
    state: &Arc<ServerState>,
    shared_builds: &[DerivationId],
) -> Result<ProbePlan> {
    if shared_builds.is_empty() {
        return Ok(ProbePlan::default());
    }

    let db = &state.worker_db;
    let outputs = gradient_db::fetch_in_chunks(shared_builds, |chunk| async move {
        EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.is_in(chunk))
            .all(db)
            .await
    })
    .await
    .context("load the outputs of the shared builds that became wanted")?;
    let recorded: HashSet<DerivationId> = outputs.iter().map(|o| o.derivation).collect();
    let answered: Vec<DerivationId> = shared_builds
        .iter()
        .copied()
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
    let mut owners: HashMap<String, Vec<DerivationId>> = HashMap::new();
    for o in outputs.iter().filter(|o| !o.is_cached_anywhere()) {
        let Some(evaluation) = evaluation_of.get(&o.derivation) else {
            continue;
        };
        let sp = StorePath::from_parts(o.hash.clone(), o.package.clone());
        by_evaluation
            .entry(*evaluation)
            .or_default()
            .insert(o.hash.clone(), sp.full());
        owners.entry(o.hash.clone()).or_default().push(o.derivation);
    }
    if by_evaluation.is_empty() {
        return Ok(ProbePlan {
            rounds: Vec::new(),
            answered,
            owners,
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

    Ok(ProbePlan {
        rounds,
        answered,
        owners,
    })
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
    async fn a_shared_build_with_an_unanswered_output_is_asked_again() {
        let silent = DerivationId::now_v7();
        let missed = DerivationId::now_v7();
        let evaluation = EvaluationId::now_v7();
        let job = |derivation| MBuildJob {
            id: gradient_types::ids::BuildJobId::now_v7(),
            evaluation,
            derivation,
            ..Default::default()
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                output(silent, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", false),
                output(missed, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", false),
            ]])
            .append_query_results([vec![job(silent), job(missed)]])
            .append_query_results([vec![MEvaluation {
                id: evaluation,
                ..Default::default()
            }]])
            .into_connection();

        let plan = plan_probes(&test_state(db), &[silent, missed])
            .await
            .expect("the plan is read");

        assert_eq!(
            plan.answered_except(&HashSet::from([
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned()
            ])),
            vec![missed],
            "only a miss every upstream answered is a reason to build"
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

        probe_pass(&state, &Mutex::new(None), &Mutex::new(Instant::now()))
            .await
            .expect("an idle tick is a no-op");
    }

    #[tokio::test]
    async fn an_unnamed_shared_build_is_not_probed() {
        let derivation = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
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
    async fn only_a_wanted_shared_build_the_probe_has_not_answered_is_asked() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MDerivationBuild>::new()])
            .into_connection();
        let log = db.clone();

        let asked = unprobed_wanted(&test_state(db), &[DerivationId::now_v7()])
            .await
            .expect("the wanted shared builds are read");

        assert!(asked.is_empty());
        let sql = log.into_transaction_log()[0].statements()[0].to_string();
        assert!(
            sql.contains(r#""derivation_build"."wanted" = TRUE"#),
            "{sql}"
        );
        assert!(
            sql.contains(r#""derivation_build"."probed" = FALSE"#),
            "{sql}"
        );
    }

    #[tokio::test]
    async fn the_unanswered_dependencies_of_a_probed_shared_build_join_its_round() {
        use gradient_entity::derivation_dependency::Model as MDerivationDependency;

        let (parent, missed, answered) = (
            DerivationId::now_v7(),
            DerivationId::now_v7(),
            DerivationId::now_v7(),
        );
        let edge = |dependency| MDerivationDependency {
            derivation: parent,
            dependency,
            ..Default::default()
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![edge(missed), edge(answered), edge(parent)]])
            .append_query_results([vec![wanted(missed), wanted(parent)]])
            .into_connection();
        let log = db.clone();

        let ahead = unprobed_dependencies(&test_state(db), &[parent])
            .await
            .expect("the dependencies are read");

        assert_eq!(ahead, vec![missed]);
        let sql = log.into_transaction_log()[1].statements()[0].to_string();
        assert!(
            sql.contains(r#""derivation_build"."probed" = FALSE"#),
            "{sql}"
        );
        assert!(
            sql.contains(r#""derivation_build"."cache_available" = FALSE"#),
            "{sql}"
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
