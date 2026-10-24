/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use gradient_db::evaluations::imports::ImportBuild;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_graph::Transition;
use gradient_pool::session_port::SessionSignal;
use gradient_types::*;
use gradient_wire::types::ImportOutcome;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tracing::{debug, warn};

use crate::Scheduler;
use crate::actor::SchedulerMsg;
use crate::buildability::{BuildWorkerCaps, BuildabilityChecker};
use crate::import_waits::{ImportAnswer, ImportWait, ImportedBuild};

pub(crate) const NO_ENTRY_POINT: &str = "no entry point recorded for the import";

impl Scheduler {
    pub async fn handle_import_request(
        &self,
        worker_id: &str,
        job_id: &str,
        evaluation: EvaluationId,
        request_id: String,
        drv_paths: Vec<String>,
    ) -> Result<()> {
        let answer = |outcome| ImportAnswer {
            worker: worker_id.to_owned(),
            job_id: job_id.to_owned(),
            request_id: request_id.clone(),
            derivations: Vec::new(),
            outcome,
        };
        let builds = match self
            .requeued_imported_builds(evaluation, &drv_paths)
            .await?
        {
            Ok(builds) => builds,
            Err(outcome) => {
                warn!(%job_id, ?outcome, "import request answered before any wait");
                self.answer_imports(vec![answer(outcome)]).await;
                return Ok(());
            }
        };

        let statuses = builds
            .iter()
            .map(|(build, row)| (build.derivation_build, row.status))
            .collect();
        let wait = ImportWait {
            worker: worker_id.to_owned(),
            job_id: job_id.to_owned(),
            request_id: request_id.clone(),
            evaluation,
            builds: builds.into_iter().map(|(build, _)| build).collect(),
            since: gradient_types::now(),
        };
        let answers = self.import_waits.register_and_settle(wait, &statuses).await;
        if answers.is_empty() {
            debug!(%job_id, %request_id, "import request waits for its builds");
            self.build_kick.kick();
        }

        self.answer_imports(answers).await;
        Ok(())
    }

    async fn requeued_imported_builds(
        &self,
        evaluation: EvaluationId,
        drv_paths: &[String],
    ) -> Result<Result<Vec<(ImportedBuild, ImportBuild)>, ImportOutcome>> {
        let builds = match self.imported_builds(evaluation, drv_paths).await? {
            Ok(builds) => builds,
            Err(outcome) => return Ok(Err(outcome)),
        };
        let Some(requeue) = requeue_of(evaluation, &builds) else {
            return Ok(Ok(builds));
        };

        self.graph
            .transition(requeue)
            .await
            .context("requeue the builds of an import")?;
        self.imported_builds(evaluation, drv_paths).await
    }

    async fn imported_builds(
        &self,
        evaluation: EvaluationId,
        drv_paths: &[String],
    ) -> Result<Result<Vec<(ImportedBuild, ImportBuild)>, ImportOutcome>> {
        let hashes: Vec<Option<String>> = drv_paths
            .iter()
            .map(|p| {
                StorePath::parse_stored(p)
                    .ok()
                    .map(|sp| sp.hash().to_owned())
            })
            .collect();
        let rows = gradient_db::evaluations::imports::import_builds(
            &self.db.worker_db,
            evaluation,
            hashes.iter().flatten().cloned().collect(),
        )
        .await
        .context("read the builds of an import request")?;

        let mut builds = Vec::with_capacity(drv_paths.len());
        for (drv_path, hash) in drv_paths.iter().zip(&hashes) {
            let Some(row) = hash.as_ref().and_then(|h| rows.get(h)) else {
                return Ok(Err(ImportOutcome::Unknown {
                    drv_path: drv_path.clone(),
                }));
            };
            if !row.has_entry_point {
                return Ok(Err(ImportOutcome::Failed {
                    drv_path: drv_path.clone(),
                    build_id: row.build_id.to_string(),
                    status: NO_ENTRY_POINT.to_owned(),
                }));
            }

            builds.push((
                ImportedBuild {
                    derivation: row.derivation,
                    derivation_build: row.derivation_build,
                    build_id: row.build_id,
                    drv_path: drv_path.clone(),
                },
                *row,
            ));
        }

        Ok(Ok(builds))
    }

    pub(crate) async fn settle_import_waits(&self) -> Result<()> {
        let Some((builds, evaluations)) = self.import_waits.waited_builds_and_evaluations().await
        else {
            return Ok(());
        };

        let db = &self.db.worker_db;
        let shared_builds = gradient_db::fetch_in_chunks(&builds, |chunk| async move {
            EDerivationBuild::find()
                .filter(CDerivationBuild::Id.is_in(chunk))
                .all(db)
                .await
        })
        .await
        .context("read the status of imported builds")?;
        let evaluations = gradient_db::fetch_in_chunks(&evaluations, |chunk| async move {
            EEvaluation::find()
                .filter(CEvaluation::Id.is_in(chunk))
                .all(db)
                .await
        })
        .await
        .context("read the evaluations waiting on imports")?;

        let statuses: HashMap<DerivationBuildId, _> =
            shared_builds.iter().map(|b| (b.id, b.status)).collect();
        let finished: HashSet<EvaluationId> = evaluations
            .iter()
            .filter(|e| EvaluationStatus::TERMINAL.contains(&e.status))
            .map(|e| e.id)
            .collect();
        let mut answers = self
            .import_waits
            .drop_finished_and_settle(&finished, &statuses)
            .await;
        answers.extend(
            self.answer_unbuildable_imports(&shared_builds, &evaluations)
                .await?,
        );
        self.answer_imports(answers).await;
        self.refresh_import_lift().await
    }

    async fn answer_unbuildable_imports(
        &self,
        shared_builds: &[MDerivationBuild],
        evaluations: &[MEvaluation],
    ) -> Result<Vec<ImportAnswer>> {
        let before = gradient_types::now()
            - chrono::Duration::seconds(crate::unbuildable::UNBUILDABLE_GRACE_SECS);
        let overdue: HashSet<DerivationBuildId> = self
            .import_waits
            .overdue_builds(before)
            .await
            .into_iter()
            .collect();
        let candidates: Vec<MDerivationBuild> = shared_builds
            .iter()
            .filter(|b| overdue.contains(&b.id))
            .cloned()
            .collect();
        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let worker_caps: Vec<BuildWorkerCaps> = self
            .board_workers()
            .await
            .iter()
            .map(BuildWorkerCaps::from)
            .collect();
        let checker = BuildabilityChecker::load(&self.db.worker_db, &candidates).await?;
        let unbuildable: HashMap<DerivationBuildId, String> = checker
            .unbuildable_imports(&candidates, &worker_caps)
            .into_iter()
            .map(|(build, unmet)| (build, unbuildable_reason(&unmet)))
            .collect();
        if unbuildable.is_empty() {
            return Ok(Vec::new());
        }

        let waiting_for_workers = self.evaluations_waiting_for_workers(evaluations).await?;
        Ok(self
            .import_waits
            .answer_unbuildable(&unbuildable, &waiting_for_workers, before)
            .await)
    }

    async fn evaluations_waiting_for_workers(
        &self,
        evaluations: &[MEvaluation],
    ) -> Result<HashSet<EvaluationId>> {
        let tasks: Vec<TaskId> = evaluations
            .iter()
            .filter_map(|e| e.task)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        if tasks.is_empty() {
            return Ok(HashSet::new());
        }

        let db = &self.db.worker_db;
        let waiting: HashSet<TaskId> = gradient_db::fetch_in_chunks(&tasks, |chunk| async move {
            ETask::find()
                .filter(CTask::Id.is_in(chunk))
                .filter(CTask::WaitForWorkers.eq(true))
                .all(db)
                .await
        })
        .await
        .context("read the tasks waiting for workers")?
        .into_iter()
        .map(|t| t.id)
        .collect();

        Ok(evaluations
            .iter()
            .filter(|e| e.task.is_some_and(|t| waiting.contains(&t)))
            .map(|e| e.id)
            .collect())
    }

    async fn refresh_import_lift(&self) -> Result<()> {
        let tracked = self
            .call(|reply| SchedulerMsg::PendingSharedBuilds { reply })
            .await?;
        if tracked.is_empty() {
            return Ok(());
        }

        let waited = self.import_waits.waited_builds().await;
        let lifted = gradient_db::scheduling::priority::import_lifted_shared_builds(
            &self.db.worker_db,
            &waited,
        )
        .await
        .context("read the builds an import lifts")?;
        let checked = tracked.into_iter().collect();
        self.call(|reply| SchedulerMsg::LiftImports {
            checked,
            lifted,
            reply,
        })
        .await?;
        Ok(())
    }

    pub(crate) async fn drop_import_waits(&self, evaluation: EvaluationId) {
        let dropped = self
            .import_waits
            .drop_evaluations(&HashSet::from([evaluation]))
            .await;
        if dropped > 0 {
            debug!(%evaluation, dropped, "dropped the import waits of an aborted evaluation");
        }
    }

    pub(crate) async fn has_import_waits(&self) -> bool {
        !self.import_waits.is_empty().await
    }

    async fn answer_imports(&self, mut answers: Vec<ImportAnswer>) {
        if answers.is_empty() {
            return;
        }

        if let Err(e) = self.add_realisations(&mut answers).await {
            warn!(error = %e, "completed imports are answered without their realised outputs");
        }

        let signals = answers
            .into_iter()
            .map(|a| {
                (
                    a.worker,
                    SessionSignal::ImportResult {
                        job_id: a.job_id,
                        request_id: a.request_id,
                        outcome: a.outcome,
                    },
                )
            })
            .collect();
        if let Err(e) = self
            .call(|reply| SchedulerMsg::SignalWorkers { signals, reply })
            .await
        {
            warn!(error = %e, "import results did not reach the scheduler");
        }
    }

    async fn add_realisations(&self, answers: &mut [ImportAnswer]) -> Result<()> {
        let completed: Vec<DerivationId> = answers
            .iter()
            .filter(|a| matches!(a.outcome, ImportOutcome::Completed { .. }))
            .flat_map(|a| a.derivations.iter().copied())
            .collect();
        if completed.is_empty() {
            return Ok(());
        }

        let realised =
            crate::realisations::realisations_by_derivation(&self.db.worker_db, &completed).await?;
        for answer in answers {
            if let ImportOutcome::Completed { realisations } = &mut answer.outcome {
                realisations.extend(
                    answer
                        .derivations
                        .iter()
                        .filter_map(|d| realised.get(d))
                        .flatten()
                        .cloned(),
                );
            }
        }

        Ok(())
    }
}

fn requeue_of(
    evaluation: EvaluationId,
    builds: &[(ImportedBuild, ImportBuild)],
) -> Option<Transition> {
    let failed = builds
        .iter()
        .any(|(_, row)| BuildStatus::REQUEUEABLE.contains(&row.status));
    let without_output_paths: Vec<DerivationId> = builds
        .iter()
        .filter(|(_, row)| row.finished_without_output_path())
        .map(|(_, row)| row.derivation)
        .collect();
    if !failed && without_output_paths.is_empty() {
        return None;
    }

    let derivations = if failed {
        builds.iter().map(|(_, row)| row.derivation).collect()
    } else {
        Vec::new()
    };

    Some(Transition::RequeueImports {
        evaluation,
        derivations,
        without_output_paths,
    })
}

fn unbuildable_reason(unmet: &UnmetRequirement) -> String {
    let features = if unmet.required_features.is_empty() {
        String::new()
    } else {
        format!(" with features {}", unmet.required_features.join(", "))
    };

    format!(
        "unbuildable, no connected worker provides {}{features}",
        unmet.architecture
    )
}
