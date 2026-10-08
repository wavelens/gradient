/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_pool::session_port::SessionSignal;
use gradient_types::*;
use gradient_wire::types::ImportOutcome;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tracing::{debug, warn};

use crate::Scheduler;
use crate::actor::SchedulerMsg;
use crate::buildability::BuildabilityChecker;
use crate::import_waits::{ImportAnswer, ImportWait, ImportedBuild};

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
            outcome,
        };
        let builds = match self.imported_builds(evaluation, &drv_paths).await? {
            Ok(builds) => builds,
            Err(drv_path) => {
                warn!(%job_id, %drv_path, "import request names a derivation the evaluation never recorded");
                self.answer_imports(vec![answer(ImportOutcome::Unknown { drv_path })])
                    .await;
                return Ok(());
            }
        };

        let statuses = builds
            .iter()
            .map(|(build, status)| (build.derivation_build, *status))
            .collect();
        let wait = ImportWait {
            worker: worker_id.to_owned(),
            job_id: job_id.to_owned(),
            request_id: request_id.clone(),
            evaluation,
            builds: builds.into_iter().map(|(build, _)| build).collect(),
            since: gradient_types::now(),
        };
        let answers = {
            let mut waits = self.import_waits.lock();
            waits.register(wait);
            waits.settle(&statuses)
        };
        if answers.is_empty() {
            debug!(%job_id, %request_id, "import request waits for its builds");
            self.kick_assigner();
        }

        self.answer_imports(answers).await;
        Ok(())
    }

    async fn imported_builds(
        &self,
        evaluation: EvaluationId,
        drv_paths: &[String],
    ) -> Result<Result<Vec<(ImportedBuild, gradient_entity::build::BuildStatus)>, String>> {
        let hashes: Vec<Option<String>> = drv_paths
            .iter()
            .map(|p| StorePath::parse(p).ok().map(|sp| sp.hash().to_owned()))
            .collect();
        let rows = gradient_db::evaluations::imports::import_builds(
            &self.state.worker_db,
            evaluation,
            hashes.iter().flatten().cloned().collect(),
        )
        .await
        .context("read the builds of an import request")?;

        let mut builds = Vec::with_capacity(drv_paths.len());
        for (drv_path, hash) in drv_paths.iter().zip(&hashes) {
            let Some(row) = hash.as_ref().and_then(|h| rows.get(h)) else {
                return Ok(Err(drv_path.clone()));
            };
            builds.push((
                ImportedBuild {
                    derivation_build: row.derivation_build,
                    build_id: row.build_id,
                    drv_path: drv_path.clone(),
                },
                row.status,
            ));
        }

        Ok(Ok(builds))
    }

    pub(crate) async fn settle_import_waits(&self) -> Result<()> {
        let (builds, evaluations) = {
            let waits = self.import_waits.lock();
            if waits.is_empty() {
                return Ok(());
            }
            (waits.waited_builds(), waits.waiting_evaluations())
        };

        let db = &self.state.worker_db;
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
        let mut answers = {
            let mut waits = self.import_waits.lock();
            waits.drop_evaluations(&finished);
            waits.settle(&statuses)
        };
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
            .lock()
            .overdue_builds(before)
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

        let worker_caps: Vec<(Vec<String>, Vec<String>)> = self
            .board_workers()
            .await
            .into_iter()
            .map(|w| (w.architectures, w.system_features))
            .collect();
        let checker = BuildabilityChecker::load(&self.state, &candidates).await?;
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
            .lock()
            .answer_unbuildable(&unbuildable, &waiting_for_workers, before))
    }

    async fn evaluations_waiting_for_workers(
        &self,
        evaluations: &[MEvaluation],
    ) -> Result<HashSet<EvaluationId>> {
        let tasks: HashSet<TaskId> = evaluations.iter().filter_map(|e| e.task).collect();
        if tasks.is_empty() {
            return Ok(HashSet::new());
        }

        let waiting: HashSet<TaskId> = ETask::find()
            .filter(CTask::Id.is_in(tasks))
            .filter(CTask::WaitForWorkers.eq(true))
            .all(&self.state.worker_db)
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

        let lifted = gradient_db::scheduling::priority::import_lifted_shared_builds(
            &self.state.worker_db,
            &tracked,
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

    pub(crate) fn drop_import_waits(&self, evaluation: EvaluationId) {
        let dropped = self
            .import_waits
            .lock()
            .drop_evaluations(&HashSet::from([evaluation]));
        if dropped > 0 {
            debug!(%evaluation, dropped, "dropped the import waits of an aborted evaluation");
        }
    }

    pub(crate) fn has_import_waits(&self) -> bool {
        !self.import_waits.lock().is_empty()
    }

    async fn answer_imports(&self, answers: Vec<ImportAnswer>) {
        if answers.is_empty() {
            return;
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
