/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_pool::session_port::SessionSignal;
use gradient_pool::{InputRequest, WorkerCaps, WorkerInfo};
use gradient_types::*;
use gradient_wire::types::InputOutcome;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tracing::debug;

use crate::Scheduler;
use crate::input_waits::{AssignedFetch, InputWaiter, NewInputWait, UnassignedFetch};
use crate::jobs::PendingJob;

impl Scheduler {
    pub async fn handle_input_request(
        &self,
        worker_id: &str,
        job_id: &str,
        request_id: String,
        request: String,
    ) -> Result<()> {
        let Some(PendingJob::Eval(job)) = self.active_job(job_id).await else {
            debug!(%worker_id, %job_id, "input request from a job that is no running evaluation; dropped");
            return Ok(());
        };
        let waiter = InputWaiter {
            worker: worker_id.to_owned(),
            job_id: job_id.to_owned(),
            request_id,
            evaluation: job.evaluation_id,
        };
        let parsed = match InputRequest::parse(&request) {
            Ok(parsed) => parsed,
            Err(message) => {
                self.answer_inputs(vec![waiter], InputOutcome::Failed { message })
                    .await;
                return Ok(());
            }
        };

        self.input_waits.register(NewInputWait {
            fetch_key: parsed.fetch_key,
            request,
            project: job.project_id,
            needs_secrets: parsed.needs_secrets,
            waiter,
        });
        self.dispatch_input_fetches().await;
        Ok(())
    }

    pub async fn handle_input_fetched(
        &self,
        worker_id: &str,
        fetch_id: &str,
        outcome: InputOutcome,
    ) {
        let waiters = self.input_waits.take_fetched(worker_id, fetch_id);
        if waiters.is_empty() {
            debug!(%worker_id, %fetch_id, "input fetch answered without waiters; dropped");
            return;
        }

        self.answer_inputs(waiters, outcome).await;
    }

    pub async fn handle_input_fetch_progress(
        &self,
        worker_id: &str,
        fetch_id: &str,
        bytes_done: u64,
        bytes_total: Option<u64>,
    ) {
        let signals = self
            .input_waits
            .waiters_of_fetch(worker_id, fetch_id)
            .into_iter()
            .map(|w| {
                (
                    w.worker,
                    SessionSignal::InputProgress {
                        job_id: w.job_id,
                        request_id: w.request_id,
                        bytes_done,
                        bytes_total,
                    },
                )
            })
            .collect::<Vec<_>>();
        if !signals.is_empty() {
            self.signal_workers(signals).await;
        }
    }

    pub(crate) async fn reassign_input_fetches_of(&self, worker_id: &str) {
        self.input_waits.release_worker(worker_id);
        self.dispatch_input_fetches().await;
    }

    pub(crate) fn drop_input_waits(&self, evaluation: EvaluationId) {
        let dropped = self
            .input_waits
            .drop_evaluations(&HashSet::from([evaluation]));
        if dropped > 0 {
            debug!(%evaluation, dropped, "dropped the input waits of an aborted evaluation");
        }
    }

    pub(crate) async fn settle_input_waits(&self) -> Result<()> {
        let waiting: Vec<EvaluationId> =
            self.input_waits.waiting_evaluations().into_iter().collect();
        if waiting.is_empty() {
            return Ok(());
        }

        let db = &self.db.worker_db;
        let finished: HashSet<EvaluationId> =
            gradient_db::fetch_in_chunks(&waiting, |chunk| async move {
                EEvaluation::find()
                    .filter(CEvaluation::Id.is_in(chunk))
                    .filter(CEvaluation::Status.is_in(EvaluationStatus::TERMINAL.to_vec()))
                    .all(db)
                    .await
            })
            .await
            .context("read the evaluations waiting on inputs")?
            .into_iter()
            .map(|e| e.id)
            .collect();
        self.input_waits.drop_evaluations(&finished);
        self.dispatch_input_fetches().await;
        Ok(())
    }

    pub(crate) fn input_waiting_evaluations(&self) -> HashSet<EvaluationId> {
        self.input_waits.unassigned_evaluations()
    }

    async fn dispatch_input_fetches(&self) {
        let unassigned = self.input_waits.unassigned();
        if unassigned.is_empty() {
            return;
        }

        let workers = self.board_workers().await;
        let mut load = self.input_waits.assigned_per_worker();
        let mut signals = Vec::new();
        for fetch in unassigned {
            let Some(worker) = fetch_worker(&workers, &load, &fetch) else {
                debug!(fetch_key = %fetch.fetch_key, "no connected worker can fetch the input yet");
                continue;
            };
            let fetch_id = uuid::Uuid::now_v7().to_string();
            let assigned = AssignedFetch {
                fetch_id: fetch_id.clone(),
                worker: worker.clone(),
            };
            if self.input_waits.assign(&fetch.fetch_key, assigned) {
                *load.entry(worker.clone()).or_default() += 1;
                signals.push((
                    worker,
                    SessionSignal::FetchInput {
                        fetch_id,
                        project: fetch.project,
                        request: fetch.request,
                    },
                ));
            }
        }
        if !signals.is_empty() {
            self.signal_workers(signals).await;
        }
    }

    async fn answer_inputs(&self, waiters: Vec<InputWaiter>, outcome: InputOutcome) {
        let signals = waiters
            .into_iter()
            .map(|w| {
                (
                    w.worker,
                    SessionSignal::InputResult {
                        job_id: w.job_id,
                        request_id: w.request_id,
                        outcome: outcome.clone(),
                    },
                )
            })
            .collect();
        self.signal_workers(signals).await;
    }
}

fn fetch_worker(
    workers: &[WorkerInfo],
    load: &HashMap<String, usize>,
    fetch: &UnassignedFetch,
) -> Option<String> {
    workers
        .iter()
        .filter(|w| !w.draining)
        .filter(|w| {
            w.authorized_peers
                .as_ref()
                .is_none_or(|peers| peers.contains(&fetch.project))
        })
        .filter(|w| {
            WorkerCaps {
                capabilities: w.capabilities.clone(),
                ..WorkerCaps::default()
            }
            .can_fetch_input(fetch.needs_secrets)
        })
        .min_by_key(|w| (load.get(&w.id).copied().unwrap_or_default(), w.id.clone()))
        .map(|w| w.id.clone())
}
