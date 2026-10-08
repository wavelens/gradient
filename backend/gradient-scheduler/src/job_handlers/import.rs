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
        let statuses: HashMap<DerivationBuildId, _> =
            gradient_db::fetch_in_chunks(&builds, |chunk| async move {
                EDerivationBuild::find()
                    .filter(CDerivationBuild::Id.is_in(chunk))
                    .all(db)
                    .await
            })
            .await
            .context("read the status of imported builds")?
            .into_iter()
            .map(|b| (b.id, b.status))
            .collect();
        let finished: HashSet<EvaluationId> =
            gradient_db::fetch_in_chunks(&evaluations, |chunk| async move {
                EEvaluation::find()
                    .filter(CEvaluation::Id.is_in(chunk))
                    .filter(CEvaluation::Status.is_in(EvaluationStatus::TERMINAL))
                    .all(db)
                    .await
            })
            .await
            .context("read the status of evaluations waiting on imports")?
            .into_iter()
            .map(|e| e.id)
            .collect();

        let answers = {
            let mut waits = self.import_waits.lock();
            waits.drop_evaluations(&finished);
            waits.settle(&statuses)
        };
        self.answer_imports(answers).await;
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
