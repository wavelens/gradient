/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Task executors — one module per job task type.
//!
//! [`JobExecutor`] is the top-level orchestrator that dispatches to the
//! appropriate sub-executor based on the job type received from the server.

pub mod build;
pub mod compress;
pub mod eval;
pub mod fetch;
pub mod sign;

use anyhow::Result;
use proto::messages::{BuildJob, FlakeJob, FlakeTask};
use tracing::instrument;

use crate::credentials::CredentialStore;
use crate::job::JobUpdater;
use crate::store::LocalNixStore;

pub use eval::WorkerEvaluator;

/// Executes jobs dispatched by the server.
///
/// Each method corresponds to one `Job` variant from the proto spec.
/// Results and status updates are sent back through [`JobUpdater`].
pub struct JobExecutor {
    pub(crate) store: LocalNixStore,
    pub(crate) evaluator: WorkerEvaluator,
    pub(crate) credentials: CredentialStore,
}

impl JobExecutor {
    pub fn new(store: LocalNixStore, evaluator: WorkerEvaluator, credentials: CredentialStore) -> Self {
        Self { store, evaluator, credentials }
    }

    /// Execute a `FlakeJob` (fetch → eval-flake → eval-derivations).
    #[instrument(skip_all, fields(tasks = ?job.tasks))]
    pub async fn execute_flake_job(
        &self,
        job: FlakeJob,
        updater: &mut JobUpdater<'_>,
        credentials: &CredentialStore,
    ) -> Result<()> {
        for task in &job.tasks {
            match task {
                FlakeTask::FetchFlake => fetch::fetch_repository(&job, updater as &mut dyn proto::traits::JobReporter, credentials).await?,
                FlakeTask::EvaluateFlake => eval::evaluate_flake(&job, updater).await?,
                FlakeTask::EvaluateDerivations => {
                    eval::evaluate_derivations(&self.evaluator, &self.store, &job, updater)
                        .await?;
                }
            }
        }
        Ok(())
    }

    /// Execute a `BuildJob` (builds → compress → sign).
    #[instrument(skip_all)]
    pub async fn execute_build_job(
        &self,
        job: BuildJob,
        updater: &mut JobUpdater<'_>,
    ) -> Result<()> {
        for build_task in &job.builds {
            build::build_derivation(&self.store, build_task, updater).await?;
        }
        if let Some(compress_task) = &job.compress {
            compress::compress_outputs(&self.store, compress_task, updater).await?;
        }
        if let Some(sign_task) = &job.sign {
            sign::sign_outputs(&self.store, &self.credentials, sign_task, updater).await?;
        }
        Ok(())
    }
}
