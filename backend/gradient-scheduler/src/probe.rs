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
use gradient_util::supervision::{Pass, PassError};
use sea_orm::sea_query::Query;
use sea_orm::{
    ColumnTrait, EntityTrait, JoinType, QueryFilter, QueryOrder, QuerySelect, QueryTrait,
    RelationTrait,
};
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
    failed_probe_build_ids: Mutex<HashSet<DerivationId>>,
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
            failed_probe_build_ids: Mutex::default(),
        }
    }

    async fn probe_pass(&self) -> Result<()> {
        let started = Instant::now();
        let mut requested = drain_requests(&self.inbox).await;
        requested.extend(self.failed_probe_build_ids.lock().await.drain());
        let mut sweeping = requested.is_empty() && sweep_due(&self.swept).await;
        let mut swept_up_to = None;

        loop {
            if requested.is_empty() && sweeping {
                let page = unanswered_need(&self.db, swept_up_to).await?;
                sweeping = page.len() == PROBE_BATCH;
                swept_up_to = page.last().copied();
                requested.extend(page);
            }
            if requested.is_empty() {
                return Ok(());
            }

            let mut shared_builds: Vec<DerivationId> = requested.drain().collect();
            shared_builds.sort_unstable();
            if let Err(e) = self.probe_round(shared_builds.clone()).await {
                self.failed_probe_build_ids
                    .lock()
                    .await
                    .extend(shared_builds);
                return Err(e);
            }
            if started.elapsed() > PROBE_DESCENT {
                return Ok(());
            }

            requested = drain_requests(&self.inbox).await;
        }
    }

    async fn probe_round(&self, shared_builds: Vec<DerivationId>) -> Result<()> {
        let mut targets = wanted_uncached(&self.db, &shared_builds).await?;
        targets.extend(unprobed_dependencies(&self.db, &targets).await?);
        let plan = plan_probes(&self.db, &targets).await?;
        let mut unanswered_outputs: HashSet<String> = HashSet::new();
        let mut upstream_found_outputs: HashSet<String> = HashSet::new();
        for (evaluation, targets) in &plan.rounds {
            let outputs_missing_from_upstream_cache: Vec<StorePath> = targets
                .iter()
                .filter(|path| !upstream_found_outputs.contains(path.hash()))
                .cloned()
                .collect();
            for chunk in outputs_missing_from_upstream_cache.chunks(PROBE_BATCH) {
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
                upstream_found_outputs.extend(answer.hits.keys().cloned());
                self.graph
                    .upstream_hits(answer.hits)
                    .await
                    .context("apply what the upstream probe found")?;
            }
        }
        unanswered_outputs.retain(|hash| !upstream_found_outputs.contains(hash));

        self.graph
            .upstream_probed(plan.answered_except(&unanswered_outputs))
            .await
            .context("record the shared builds this round answered for")
    }
}

impl Pass for UpstreamProbe {
    const NAME: &'static str = "upstream-probe";
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
async fn unanswered_need(db: &WorkerDb, after: Option<DerivationId>) -> Result<Vec<DerivationId>> {
    Ok(unanswered_need_query(after)
        .all(db)
        .await
        .context("find needed builds the upstream probe was never asked about")?
        .into_iter()
        .map(|a| a.derivation)
        .collect())
}

fn unanswered_need_query(after: Option<DerivationId>) -> sea_orm::Select<EDerivationBuild> {
    let named = Query::select()
        .column(CBuildJob::DerivationBuild)
        .from(EBuildJob::default())
        .to_owned();
    EDerivationBuild::find()
        .join(
            JoinType::InnerJoin,
            gradient_entity::derivation_build::Relation::Derivation.def(),
        )
        .filter(CDerivationBuild::Probed.eq(false))
        .filter(CDerivationBuild::Wanted.eq(true))
        .filter(CDerivation::Walked.eq(true))
        .filter(CDerivationBuild::Id.in_subquery(named))
        .apply_if(after, |query, after| {
            query.filter(CDerivationBuild::Derivation.gt(after))
        })
        .order_by_asc(CDerivationBuild::Derivation)
        .limit(PROBE_BATCH as u64)
}

#[derive(Debug, Default)]
pub(crate) struct ProbePlan {
    pub rounds: Vec<(MEvaluation, Vec<StorePath>)>,
    pub asked: Vec<DerivationId>,
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
        self.asked
            .iter()
            .copied()
            .filter(|d| !waiting.contains(d))
            .collect()
    }
}

/// A miss from an earlier need is stale. An upstream cache can gain the outputs between two
/// evaluations, so every new need asks again.
async fn wanted_uncached(
    db: &WorkerDb,
    shared_builds: &[DerivationId],
) -> Result<Vec<DerivationId>> {
    Ok(
        gradient_db::fetch_in_chunks(shared_builds, |chunk| async move {
            EDerivationBuild::find()
                .filter(CDerivationBuild::Derivation.is_in(chunk))
                .filter(CDerivationBuild::Wanted.eq(true))
                .filter(CDerivationBuild::CacheAvailable.eq(false))
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
    let cached = gradient_db::derivation_outputs::cached_anywhere(db, &outputs)
        .await
        .context("find the outputs some cache already holds")?;
    let to_ask: Vec<&MDerivationOutput> =
        outputs.iter().filter(|o| !cached.contains(&o.id)).collect();
    let newest_evaluations_by_derivation_id =
        newest_evaluation_per_naming_project(db, &to_ask).await?;

    let mut plan = ProbePlan::default();
    let mut by_evaluation: HashMap<EvaluationId, (MEvaluation, HashMap<String, StorePath>)> =
        HashMap::new();
    let mut unaskable: HashSet<DerivationId> = HashSet::new();
    for o in to_ask {
        let Some(evaluations) = newest_evaluations_by_derivation_id.get(&o.derivation) else {
            unaskable.insert(o.derivation);
            continue;
        };
        let Some(path) = o.store_path() else {
            continue;
        };
        let hash = path.hash().to_owned();
        for evaluation in evaluations {
            by_evaluation
                .entry(evaluation.id)
                .or_insert_with(|| (evaluation.clone(), HashMap::new()))
                .1
                .insert(hash.clone(), path.clone());
        }
        plan.owners.entry(hash).or_default().push(o.derivation);
    }

    let recorded: HashSet<DerivationId> = outputs.iter().map(|o| o.derivation).collect();
    plan.asked = shared_builds
        .iter()
        .copied()
        .filter(|d| recorded.contains(d) && !unaskable.contains(d))
        .collect();
    plan.rounds = by_evaluation
        .into_values()
        .map(|(evaluation, targets)| (evaluation, targets.into_values().collect()))
        .collect();

    Ok(plan)
}

/// Misses in the upstream caches of one project say nothing about the upstream caches of another
/// project. The probe can query every project that names the shared build, through the newest
/// evaluation of each project.
async fn newest_evaluation_per_naming_project(
    db: &WorkerDb,
    outputs: &[&MDerivationOutput],
) -> Result<HashMap<DerivationId, Vec<MEvaluation>>> {
    let mut derivations: Vec<DerivationId> = outputs.iter().map(|o| o.derivation).collect();
    derivations.sort_unstable();
    derivations.dedup();
    let jobs = gradient_db::fetch_in_chunks(&derivations, |chunk| async move {
        EBuildJob::find()
            .filter(CBuildJob::Derivation.is_in(chunk))
            .all(db)
            .await
    })
    .await
    .context("find the evaluations that name each shared build that became wanted")?;

    let mut ids: Vec<EvaluationId> = jobs.iter().map(|j| j.evaluation).collect();
    ids.sort_unstable();
    ids.dedup();
    let evaluations = gradient_db::fetch_in_chunks(&ids, |chunk| async move {
        EEvaluation::find()
            .filter(CEvaluation::Id.is_in(chunk))
            .all(db)
            .await
    })
    .await
    .context("load the evaluations the probe reads its upstream caches through")?;
    let project_id_by_evaluation_id = load_project_ids_for_evaluations(db, &evaluations).await?;

    let mut newest_evaluation_per_project: HashMap<ProjectId, MEvaluation> = HashMap::new();
    for evaluation in evaluations {
        if let Some(&project) = project_id_by_evaluation_id.get(&evaluation.id)
            && newest_evaluation_per_project
                .get(&project)
                .is_none_or(|kept| kept.id < evaluation.id)
        {
            newest_evaluation_per_project.insert(project, evaluation);
        }
    }

    let mut newest_evaluations_by_derivation: HashMap<DerivationId, Vec<MEvaluation>> =
        HashMap::new();
    let mut already_added_derivation_project_pairs: HashSet<(DerivationId, ProjectId)> =
        HashSet::new();
    for job in jobs {
        if let Some(&project) = project_id_by_evaluation_id.get(&job.evaluation)
            && already_added_derivation_project_pairs.insert((job.derivation, project))
            && let Some(evaluation) = newest_evaluation_per_project.get(&project)
        {
            newest_evaluations_by_derivation
                .entry(job.derivation)
                .or_default()
                .push(evaluation.clone());
        }
    }

    Ok(newest_evaluations_by_derivation)
}

async fn load_project_ids_for_evaluations(
    db: &WorkerDb,
    evaluations: &[MEvaluation],
) -> Result<HashMap<EvaluationId, ProjectId>> {
    let mut task_ids: Vec<TaskId> = evaluations.iter().filter_map(|e| e.task).collect();
    task_ids.sort_unstable();
    task_ids.dedup();
    let project_id_by_task_id: HashMap<TaskId, ProjectId> =
        gradient_db::fetch_in_chunks(&task_ids, |chunk| async move {
            ETask::find().filter(CTask::Id.is_in(chunk)).all(db).await
        })
        .await
        .context("Load the projects of evaluations that name a shared build")?
        .into_iter()
        .map(|t| (t.id, t.project))
        .collect();

    Ok(evaluations
        .iter()
        .filter_map(|e| Some((e.id, *project_id_by_task_id.get(&e.task?)?)))
        .collect())
}

#[cfg(test)]
impl UpstreamProbe {
    pub(crate) async fn round(&self, shared_builds: Vec<DerivationId>) -> Result<()> {
        self.probe_round(shared_builds).await
    }

    pub(crate) async fn sweep(&self) -> Result<()> {
        let need = unanswered_need(&self.db, None).await?;
        self.probe_round(need).await
    }
}

#[cfg(test)]
pub(crate) async fn planned_round(
    db: &WorkerDb,
    shared_builds: &[DerivationId],
) -> Result<ProbePlan> {
    let mut targets = wanted_uncached(db, shared_builds).await?;
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
            asked: vec![silent, missed],
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
    fn the_sweep_reads_only_walked_shared_builds_an_evaluation_names() {
        use sea_orm::QueryTrait as _;
        let sql = unanswered_need_query(None)
            .build(DatabaseBackend::Postgres)
            .to_string();
        assert!(sql.contains(r#""derivation"."walked" = TRUE"#), "{sql}");
        assert!(
            sql.contains(
                r#""derivation_build"."id" IN (SELECT "derivation_build" FROM "build_job")"#
            ),
            "{sql}"
        );
    }

    #[test]
    fn the_sweep_query_for_unasked_builds_continues_after_last_derivation_id_in_order() {
        use sea_orm::QueryTrait as _;
        let last = DerivationId::now_v7();
        let sql = unanswered_need_query(Some(last))
            .build(DatabaseBackend::Postgres)
            .to_string();
        assert!(
            sql.contains(&format!(r#""derivation_build"."derivation" > '{last}'"#)),
            "{sql}"
        );
        assert!(
            sql.contains(r#"ORDER BY "derivation_build"."derivation" ASC"#),
            "{sql}"
        );
    }
}
