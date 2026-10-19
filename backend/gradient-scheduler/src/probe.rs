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
use gradient_db::{DbContext, WorkerDb};
use gradient_entity::StorePath;
use gradient_graph::Graph;
use gradient_types::*;
use gradient_util::supervision::{Pass, PassError, RunsOn};
use sea_orm::{ColumnTrait, EntityTrait, JoinType, QueryFilter, QuerySelect, RelationTrait};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::{Mutex, Notify, Semaphore};
use tracing::debug;

const PROBE_BATCH: usize = 256;

const PROBE_SWEEP: Duration = Duration::from_secs(60);
const PROBE_DESCENT: Duration = Duration::from_secs(60);

type Inbox = Mutex<Option<UnboundedReceiver<Vec<DerivationId>>>>;

pub(crate) struct UpstreamProbe {
    db: WorkerDb,
    store_path_mapping: Arc<dyn StorePathMapping>,
    graph: Arc<Graph>,
    upstream_query: Arc<Semaphore>,
    wake: Arc<Notify>,
    inbox: Inbox,
    swept: Mutex<Instant>,
}

impl UpstreamProbe {
    pub(crate) fn new(db: &DbContext, graph: &Arc<Graph>, upstream_query: &Arc<Semaphore>) -> Self {
        Self {
            db: db.worker_db.clone(),
            store_path_mapping: Arc::clone(&db.store_path_mapping),
            graph: Arc::clone(graph),
            upstream_query: Arc::clone(upstream_query),
            wake: db.memory.probe_requests.wake(),
            inbox: Mutex::new(db.memory.probe_requests.take_inbox()),
            swept: Mutex::new(Instant::now()),
        }
    }

    async fn probe_pass(&self) -> Result<()> {
        let started = Instant::now();
        let mut requested = drain_requests(&self.inbox).await;
        if requested.is_empty() && sweep_due(&self.swept).await {
            requested.extend(unanswered_need(&self.db).await?);
        }

        while !requested.is_empty() {
            let mut shared_builds: Vec<DerivationId> = requested.into_iter().collect();
            shared_builds.sort_unstable();
            self.probe_round(shared_builds).await?;
            if started.elapsed() > PROBE_DESCENT {
                break;
            }

            requested = drain_requests(&self.inbox).await;
        }

        Ok(())
    }

    async fn probe_round(&self, shared_builds: Vec<DerivationId>) -> Result<()> {
        let mut targets = unprobed_wanted(&self.db, &shared_builds).await?;
        targets.extend(unprobed_dependencies(&self.db, &targets).await?);
        let plan = plan_probes(&self.db, &targets).await?;
        let mut unanswered_outputs: HashSet<String> = HashSet::new();
        for (evaluation, targets) in &plan.rounds {
            for chunk in targets.chunks(PROBE_BATCH) {
                let answer = crate::eval::probe_outputs(
                    &self.db,
                    &self.store_path_mapping,
                    &self.upstream_query,
                    evaluation,
                    chunk.to_vec(),
                )
                .await;
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
                self.graph
                    .upstream_hits(answer.hits)
                    .await
                    .context("apply what the upstream probe found")?;
            }
        }

        self.graph
            .upstream_probed(plan.answered_except(&unanswered_outputs))
            .await
            .context("record the shared builds this round answered for")
    }
}

impl Pass for UpstreamProbe {
    const NAME: &'static str = "upstream-probe";
    const RUNS_ON: RunsOn = RunsOn::EveryInstance;
    const BUDGET: Duration = Duration::from_secs(120);

    fn period(&self) -> Duration {
        Duration::from_secs(1)
    }

    fn wake(&self) -> Option<Arc<Notify>> {
        Some(Arc::clone(&self.wake))
    }

    async fn run(&self) -> Result<(), PassError> {
        self.probe_pass()
            .await
            .map_err(|e| PassError::from(e.to_string()))
    }
}

async fn drain_requests(inbox: &Inbox) -> HashSet<DerivationId> {
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
async fn unanswered_need(db: &WorkerDb) -> Result<Vec<DerivationId>> {
    Ok(unanswered_need_query()
        .all(db)
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
    pub rounds: Vec<(MEvaluation, Vec<StorePath>)>,
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
    db: &WorkerDb,
    shared_builds: &[DerivationId],
) -> Result<Vec<DerivationId>> {
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
    db: &WorkerDb,
    shared_builds: &[DerivationId],
) -> Result<Vec<DerivationId>> {
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
    db: &WorkerDb,
    shared_builds: &[DerivationId],
) -> Result<ProbePlan> {
    if shared_builds.is_empty() {
        return Ok(ProbePlan::default());
    }

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

    let mut by_evaluation: HashMap<EvaluationId, HashMap<String, StorePath>> = HashMap::new();
    let mut owners: HashMap<String, Vec<DerivationId>> = HashMap::new();
    let cached = gradient_db::derivation_outputs::cached_anywhere(db, &outputs)
        .await
        .context("find the outputs some cache already holds")?;
    for o in outputs.iter().filter(|o| !cached.contains(&o.id)) {
        let Some(evaluation) = evaluation_of.get(&o.derivation) else {
            continue;
        };
        by_evaluation
            .entry(*evaluation)
            .or_default()
            .insert(o.hash.clone(), StorePath::from(o.clone()));
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
                .map(|targets| (e, targets.into_values().collect()))
        })
        .collect();

    Ok(ProbePlan {
        rounds,
        answered,
        owners,
    })
}

#[cfg(test)]
impl UpstreamProbe {
    pub(crate) async fn round(&self, shared_builds: Vec<DerivationId>) -> Result<()> {
        self.probe_round(shared_builds).await
    }

    pub(crate) async fn sweep(&self) -> Result<()> {
        let need = unanswered_need(&self.db).await?;
        self.probe_round(need).await
    }
}

#[cfg(test)]
pub(crate) async fn planned_round(
    db: &WorkerDb,
    shared_builds: &[DerivationId],
) -> Result<ProbePlan> {
    let mut targets = unprobed_wanted(db, shared_builds).await?;
    targets.extend(unprobed_dependencies(db, &targets).await?);
    plan_probes(db, &targets).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::DatabaseBackend;

    #[test]
    fn a_shared_build_with_an_unanswered_output_is_asked_again() {
        let (silent, missed) = (DerivationId::now_v7(), DerivationId::now_v7());
        let plan = ProbePlan {
            rounds: Vec::new(),
            answered: vec![silent, missed],
            owners: HashMap::from([
                ("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(), vec![silent]),
                ("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(), vec![missed]),
            ]),
        };

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

    #[test]
    fn the_sweep_reads_only_walked_shared_builds() {
        use sea_orm::QueryTrait as _;
        let sql = unanswered_need_query()
            .build(DatabaseBackend::Postgres)
            .to_string();
        assert!(sql.contains(r#""derivation"."walked" = TRUE"#), "{sql}");
    }
}
