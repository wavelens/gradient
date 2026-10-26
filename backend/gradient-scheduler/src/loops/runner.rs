/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use gradient_db::runner_jobs::dispatch::{self, RunnerDispatchRow};
use gradient_graph::Transition;
use gradient_pool::{WorkerCaps, WorkerInfo};
use gradient_types::ProjectId;
use gradient_types::runner::RunnerSpec;
use gradient_util::supervision::{Pass, PassError, RunsOn};
use gradient_wire::types::RequiredPath;
use tracing::{debug, error, warn};

use crate::Scheduler;
use crate::buildability::runner_waiting_reason;
use crate::jobs::{PendingJob, PendingRunnerJob, RunnerAssignment, runner_job_key};

pub(crate) struct RunnerDispatch {
    pub(crate) scheduler: Arc<Scheduler>,
}

impl Pass for RunnerDispatch {
    const NAME: &'static str = "runner-dispatch";
    const RUNS_ON: RunsOn = RunsOn::EveryInstance;
    const BUDGET: Duration = super::ASSIGN_BUDGET;

    fn period(&self) -> Duration {
        super::ASSIGN_TICK
    }

    async fn run(&self) -> Result<(), PassError> {
        assign_runner_jobs(&self.scheduler)
            .await
            .map_err(Into::into)
    }
}

#[tracing::instrument(level = "debug", skip_all)]
pub(crate) async fn assign_runner_jobs(scheduler: &Scheduler) -> anyhow::Result<()> {
    if scheduler.draining.is_draining().await {
        return Ok(());
    }

    scheduler
        .graph
        .transition(Transition::SettleRunnerJobs)
        .await?;
    let rows = dispatch::queued_untracked(&scheduler.db.worker_db).await?;
    let keys: Vec<String> = rows.iter().map(|row| runner_job_key(row.job.id)).collect();
    let untracked: HashSet<String> = scheduler.untracked(keys).await.into_iter().collect();
    let workers = scheduler.board_workers().await;
    let api_url = scheduler.db.config.server.serve_url.clone();

    for row in rows {
        let Some(pending) = pending_runner_job(&row, &api_url) else {
            continue;
        };
        let caps = runner_workers(&workers, row.project);
        let reason = runner_waiting_reason(&pending.spec, &pending.architecture, &caps);
        dispatch::set_waiting_reason(&scheduler.db.worker_db, row.job.id, reason.as_ref()).await?;

        let key = runner_job_key(row.job.id);
        if !untracked.contains(&key) {
            continue;
        }
        if let Err(e) = scheduler
            .enqueue_routed(
                crate::cluster::Route::Single,
                key.clone(),
                PendingJob::Runner(pending),
            )
            .await
        {
            error!(error = %e, %key, "enqueueing a runner job failed");
            continue;
        }
        debug!(%key, "runner job enqueued");
    }

    Ok(())
}

pub(crate) fn caps_of(worker: &WorkerInfo) -> WorkerCaps {
    WorkerCaps {
        architectures: worker.architectures.clone(),
        system_features: worker.system_features.clone(),
        capabilities: worker.capabilities.clone(),
        runner: worker.runner.clone(),
        ..WorkerCaps::default()
    }
}

fn runner_workers(workers: &[WorkerInfo], project: ProjectId) -> Vec<WorkerCaps> {
    workers
        .iter()
        .filter(|w| !w.draining)
        .filter(|w| {
            w.authorized_peers
                .as_ref()
                .is_none_or(|peers| peers.contains(&project))
        })
        .map(caps_of)
        .collect()
}

pub(crate) fn pending_runner_job(
    row: &RunnerDispatchRow,
    api_url: &str,
) -> Option<PendingRunnerJob> {
    let spec: RunnerSpec = match serde_json::from_value(row.job.spec.clone()) {
        Ok(spec) => spec,
        Err(e) => {
            warn!(error = %e, runner_job = %row.job.id, "runner spec does not decode; job skipped");
            return None;
        }
    };
    let required_paths = std::iter::once(store_root(&spec.program))
        .chain(row.image_path.clone().map(Some))
        .flatten()
        .map(|path| RequiredPath {
            path,
            cache_info: None,
        })
        .collect();
    let now = gradient_types::now();
    Some(PendingRunnerJob {
        runner_job: row.job.id,
        evaluation_id: row.job.evaluation,
        project_id: row.project,
        task_id: row.task,
        entry_point: row.entry_point.clone(),
        repository: row.repository.clone(),
        commit: row.commit.clone(),
        architecture: row.architecture.clone(),
        image_path: row.image_path.clone(),
        retries_left: u32::try_from(row.job.retries_left).unwrap_or(0),
        attempts_done: u32::try_from(row.attempts_done).unwrap_or(0),
        last_phase: row.last_phase.clone(),
        required_paths,
        queued_at: row.job.created_at,
        ready_at: now,
        rescore_count: 0,
        api_url: api_url.to_owned(),
        assignment: row.open_attempt.map(|open| RunnerAssignment {
            runner_job: row.job.id,
            attempt: open.id,
            number: u32::try_from(open.number).unwrap_or(0),
            retries_cap: u32::try_from(row.job.retries_left).unwrap_or(0),
        }),
        spec,
    })
}

fn store_root(path: &str) -> Option<String> {
    let name = path.strip_prefix("/nix/store/")?.split('/').next()?;
    Some(format!("/nix/store/{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_requires_its_store_path() {
        assert_eq!(
            store_root("/nix/store/aaaa-train/bin/train").as_deref(),
            Some("/nix/store/aaaa-train")
        );
        assert_eq!(store_root("/bin/sh"), None);
    }
}
