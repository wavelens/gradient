/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use gradient_db::exec_jobs::dispatch::{self, ExecDispatchRow};
use gradient_graph::Transition;
use gradient_pool::{WorkerCaps, WorkerInfo};
use gradient_types::ProjectId;
use gradient_types::exec::{ExecDefaults, ExecSpec};
use gradient_util::supervision::{Pass, PassError, RunsOn};
use gradient_wire::types::RequiredPath;
use tracing::{debug, error, warn};

use crate::Scheduler;
use crate::buildability::exec_waiting_reason;
use crate::exec_secrets::{resolve_secret_names, secret_refusal};
use crate::jobs::{ExecAssignment, PendingExecJob, PendingJob, exec_job_key};

pub(crate) struct ExecDispatch {
    pub(crate) scheduler: Arc<Scheduler>,
}

impl Pass for ExecDispatch {
    const NAME: &'static str = "exec-dispatch";
    const RUNS_ON: RunsOn = RunsOn::EveryInstance;
    const BUDGET: Duration = super::ASSIGN_BUDGET;

    fn period(&self) -> Duration {
        super::ASSIGN_TICK
    }

    async fn run(&self) -> Result<(), PassError> {
        assign_exec_jobs(&self.scheduler).await.map_err(Into::into)
    }
}

#[tracing::instrument(level = "debug", skip_all)]
pub(crate) async fn assign_exec_jobs(scheduler: &Scheduler) -> anyhow::Result<()> {
    if scheduler.draining.is_draining().await {
        return Ok(());
    }

    scheduler
        .graph
        .transition(Transition::SettleExecJobs)
        .await?;
    let rows = dispatch::queued_untracked(&scheduler.db.worker_db).await?;
    let keys: Vec<String> = rows.iter().map(|row| exec_job_key(row.job.id)).collect();
    let untracked: HashSet<String> = scheduler.untracked(keys).await.into_iter().collect();
    let workers = scheduler.board_workers().await;
    let api_url = scheduler.db.config.server.serve_url.clone();

    for row in rows {
        let key = exec_job_key(row.job.id);
        let tracked = !untracked.contains(&key);
        let Some(pending) = queued_job(scheduler, &row, &key, tracked, &api_url).await? else {
            continue;
        };
        let project_workers: Vec<WorkerInfo> = workers
            .iter()
            .filter(|w| serves_project(w, row.project))
            .cloned()
            .collect();
        let requirement = pending
            .spec
            .requirement_on(&pending.architecture, &ExecDefaults::default());
        if let Some(refusal) = secret_refusal(&requirement, &pending.secrets, &project_workers) {
            fail_queued_job(scheduler, &row, &key, tracked, refusal.to_string()).await?;
            continue;
        }

        let caps: Vec<WorkerCaps> = project_workers.iter().map(caps_of).collect();
        let reason = exec_waiting_reason(&pending, &caps);
        dispatch::set_waiting_reason(&scheduler.db.worker_db, row.job.id, reason.as_ref()).await?;

        if tracked {
            continue;
        }
        if let Err(e) = scheduler
            .enqueue_routed(
                crate::cluster::Route::Single,
                key.clone(),
                PendingJob::Exec(pending),
            )
            .await
        {
            error!(error = %e, %key, "enqueueing an exec job failed");
            continue;
        }
        debug!(%key, "exec job enqueued");
    }

    scheduler.refresh_pre_warm().await;
    Ok(())
}

pub(crate) fn caps_of(worker: &WorkerInfo) -> WorkerCaps {
    WorkerCaps {
        architectures: worker.architectures.clone(),
        system_features: worker.system_features.clone(),
        capabilities: worker.capabilities.clone(),
        exec: worker.exec.clone(),
        ..WorkerCaps::default()
    }
}

pub(crate) fn serves_project(worker: &WorkerInfo, project: ProjectId) -> bool {
    !worker.draining
        && worker
            .authorized_peers
            .as_ref()
            .is_none_or(|peers| peers.contains(&project))
}

async fn queued_job(
    scheduler: &Scheduler,
    row: &ExecDispatchRow,
    key: &str,
    tracked: bool,
    api_url: &str,
) -> anyhow::Result<Option<PendingExecJob>> {
    if tracked {
        return Ok(match scheduler.pending_job(key).await {
            Some(PendingJob::Exec(pending)) => Some(pending),
            _ => None,
        });
    }

    let Some(mut pending) = pending_exec_job(row, api_url) else {
        return Ok(None);
    };
    let db = &scheduler.db;
    match resolve_secret_names(&db.worker_db, &db.config, row.project, &pending.spec).await {
        Ok(names) => {
            pending.secrets = names;
            Ok(Some(pending))
        }
        Err(refusal) => {
            fail_queued_job(scheduler, row, key, false, refusal.to_string()).await?;
            Ok(None)
        }
    }
}

async fn fail_queued_job(
    scheduler: &Scheduler,
    row: &ExecDispatchRow,
    key: &str,
    tracked: bool,
    message: String,
) -> anyhow::Result<()> {
    debug!(%key, %message, "exec job fails before dispatch");
    scheduler
        .graph
        .transition(Transition::FailExecJob {
            exec_job: row.job.id,
            message,
        })
        .await?;
    if tracked {
        scheduler
            .cancel_jobs(row.job.evaluation, vec![key.to_owned()])
            .await;
    }

    Ok(())
}

pub(crate) fn pending_exec_job(row: &ExecDispatchRow, api_url: &str) -> Option<PendingExecJob> {
    let spec: ExecSpec = match serde_json::from_value(row.job.spec.clone()) {
        Ok(spec) => spec,
        Err(e) => {
            warn!(error = %e, exec_job = %row.job.id, "exec spec does not decode; job skipped");
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
    Some(PendingExecJob {
        exec_job: row.job.id,
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
        assignment: row.open_attempt.map(|open| ExecAssignment {
            exec_job: row.job.id,
            attempt: open.id,
            number: u32::try_from(open.number).unwrap_or(0),
            retries_cap: u32::try_from(row.job.retries_left).unwrap_or(0),
        }),
        secrets: Default::default(),
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
