/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_pool::session_port::SessionSignal;
use gradient_pool::{InputRequest, WorkerCaps, WorkerInfo, WorkerJob};
use gradient_types::*;
use gradient_wire::types::InputOutcome;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tracing::{debug, warn};

use crate::Scheduler;
use crate::actor::SchedulerMsg;
use crate::exec_secrets::SecretResolutionError;
use crate::input_waits::{
    AssignedFetch, InputFetchProjectGitCredentials, InputWaiter, NewInputWait, UnassignedFetch,
};
use crate::jobs::PendingJob;
use crate::waiting_state::announce_worker_gap;

impl Scheduler {
    pub async fn handle_input_request(
        &self,
        worker_id: &str,
        job_id: &str,
        request_id: String,
        request: String,
    ) -> Result<()> {
        let Some(PendingJob::Eval(job)) = self.active_job(job_id).await else {
            debug!(%worker_id, %job_id, "input request from a job without an evaluation. answer with failure");
            let refusal = SessionSignal::InputResult {
                job_id: job_id.to_owned(),
                request_id,
                outcome: InputOutcome::Failed {
                    message: "job for this input request is not a running evaluation".to_owned(),
                },
            };
            self.signal_workers(vec![(worker_id.to_owned(), refusal)])
                .await;
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
        let needs_secrets = match self
            .input_fetch_needs_secrets_capable_worker(job.project_id, &parsed)
            .await
        {
            Ok(needs_secrets) => needs_secrets,
            Err(e) => {
                let message = format!(
                    "Server failed to query the HTTPS Git credentials of the project for the input host: {e}"
                );
                self.answer_inputs(vec![waiter], InputOutcome::Failed { message })
                    .await;
                return Ok(());
            }
        };

        let allowed = job.allows_project_credentials_and_privileged_actions;
        if parsed.needs_secrets && !allowed {
            let message =
                SecretResolutionError::ProjectGitCredentialsWithheldForUnapprovedForkPullRequest
                    .to_string();
            self.answer_inputs(vec![waiter], InputOutcome::Failed { message })
                .await;
            return Ok(());
        }
        let (project_git_credentials, fetch_key) = match (needs_secrets, allowed) {
            (false, _) => (
                InputFetchProjectGitCredentials::FetchWithoutProjectCredentials,
                format!("{}/{}", job.project_id, parsed.fetch_key),
            ),
            (true, true) => (
                InputFetchProjectGitCredentials::FetchWithProjectCredentials,
                format!("{}/{}", job.project_id, parsed.fetch_key),
            ),
            (true, false) => (
                InputFetchProjectGitCredentials::HttpsCredentialsWithheldForUnapprovedForkPullRequest,
                format!(
                    "{}/https-credentials-withheld/{}",
                    job.project_id, parsed.fetch_key
                ),
            ),
        };

        self.input_waits.register(NewInputWait {
            fetch_key,
            request: parsed.canonical_input_request_json,
            project: job.project_id,
            project_git_credentials,
            content_hash_store_path: parsed.content_hash_store_path,
            waiter,
        });
        self.dispatch_input_fetches().await;
        Ok(())
    }

    async fn input_fetch_needs_secrets_capable_worker(
        &self,
        project: ProjectId,
        request: &InputRequest,
    ) -> Result<bool, sea_orm::DbErr> {
        let pairs = match request.https_git_host {
            Some(_) if !request.needs_secrets => {
                gradient_db::project_git_https_credentials::list_project_host_pairs_with_https_git_credential_for_project_ids(
                    &self.db.worker_db,
                    &[project],
                )
                .await?
            }
            _ => HashSet::new(),
        };
        Ok(fetch_needs_secrets_capability(
            project,
            request.needs_secrets,
            request.https_git_host.as_deref(),
            &pairs,
        ))
    }

    pub async fn handle_input_fetched(
        &self,
        worker_id: &str,
        fetch_id: &str,
        outcome: InputOutcome,
    ) {
        self.release_input_fetch(worker_id, fetch_id).await;
        if let InputOutcome::Unplaced { message } = &outcome {
            if self.input_waits.unplace(worker_id, fetch_id).is_some() {
                debug!(%worker_id, %fetch_id, %message, "input fetch unplaced; trying another worker");
                self.dispatch_input_fetches().await;
            }
            return;
        }
        if let InputOutcome::Fetched { response } = &outcome
            && let Some((project, pin)) = self
                .input_waits
                .project_and_content_pinned_store_path_for_pinned_wait(worker_id, fetch_id)
        {
            let answered = read_store_path_from_worker_input_response(response);
            if answered.as_deref() != Some(pin.as_str()) {
                let answered = answered.unwrap_or_default();
                warn!(%worker_id, %project, %pin, %answered, "Worker responded to a fetch of a pinned input with a different store path. Query another worker.");
                let message = format!(
                    "The worker answered the input fetch with store path {answered}, not with the store path {pin} that the content hash of the request names."
                );
                self.exclude_worker_from_input_fetch_and_reassign_or_fail_waiting_evaluations(
                    worker_id, fetch_id, message,
                )
                .await;
                return;
            }
        }
        let Some((project_git_credentials, waiters)) = self
            .input_waits
            .take_fetched(worker_id, fetch_id)
            .filter(|(_, waiters)| !waiters.is_empty())
        else {
            debug!(%worker_id, %fetch_id, "input fetch answered without waiters; dropped");
            return;
        };
        let outcome = match outcome {
            InputOutcome::Failed { message }
                if project_git_credentials
                    == InputFetchProjectGitCredentials::HttpsCredentialsWithheldForUnapprovedForkPullRequest =>
            {
                InputOutcome::Failed {
                    message: format!(
                        "{}\n{message}",
                        SecretResolutionError::ProjectGitCredentialsWithheldForUnapprovedForkPullRequest
                    ),
                }
            }
            outcome => outcome,
        };

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
        let now = Instant::now();
        self.fail_evaluations_waiting_on_inputs_without_worker_fetch_before_timeout(now)
            .await;
        self.move_timed_out_input_fetches_to_another_worker(now)
            .await;
        self.dispatch_input_fetches().await;
        Ok(())
    }

    pub(crate) async fn fail_evaluations_waiting_on_inputs_without_worker_fetch_before_timeout(
        &self,
        now: Instant,
    ) {
        let seconds = self.db.config.scheduler.input_fetch_timeout_secs.get();
        let expired = self
            .input_waits
            .remove_waits_without_fetch_created_at_least_timeout_before_and_return_input_requests_and_waiters(
                Duration::from_secs(seconds),
                now,
            );
        for (request, waiters) in expired {
            let message =
                format!("No worker accepted the fetch of input {request} within {seconds} seconds");
            self.answer_inputs(waiters, InputOutcome::Failed { message })
                .await;
        }
    }

    pub(crate) async fn move_timed_out_input_fetches_to_another_worker(&self, now: Instant) {
        let seconds = self.db.config.scheduler.input_fetch_timeout_secs.get();
        let timed_out = self
            .input_waits
            .assigned_fetches_held_at_least_timeout(Duration::from_secs(seconds), now);
        for fetch in timed_out {
            warn!(worker = %fetch.worker, fetch_id = %fetch.fetch_id, "The input fetch received no response in time. Trying another worker.");
            self.release_input_fetch(&fetch.worker, &fetch.fetch_id)
                .await;
            self.exclude_worker_from_input_fetch_and_reassign_or_fail_waiting_evaluations(
                &fetch.worker,
                &fetch.fetch_id,
                format!("No worker responded to the input fetch within {seconds} seconds"),
            )
            .await;
        }
    }

    async fn exclude_worker_from_input_fetch_and_reassign_or_fail_waiting_evaluations(
        &self,
        worker_id: &str,
        fetch_id: &str,
        message: String,
    ) {
        let Some(fetch_key) = self.input_waits.unplace(worker_id, fetch_id) else {
            return;
        };
        let workers = match self.board_workers().await {
            Ok(workers) => workers,
            Err(e) => {
                warn!(error = %e, "Can't read connected workers. Skip assignment of input fetches this pass.");
                return;
            }
        };
        let nobody_left = self
            .input_waits
            .unassigned()
            .into_iter()
            .find(|fetch| fetch.fetch_key == fetch_key)
            .is_some_and(|fetch| !workers.iter().any(|w| can_ever_fetch(w, &fetch)));
        if !nobody_left {
            self.dispatch_input_fetches().await;
            return;
        }
        let waiters = self
            .input_waits
            .remove_wait_without_fetch_by_key_and_return_waiters(&fetch_key);
        self.answer_inputs(waiters, InputOutcome::Failed { message })
            .await;
    }

    async fn dispatch_input_fetches(&self) {
        let unassigned = self.input_waits.unassigned();
        if unassigned.is_empty() {
            return;
        }

        let mut workers = match self.board_workers().await {
            Ok(workers) => workers,
            Err(e) => {
                warn!(error = %e, "Can't read connected workers. Skip assignment of input fetches this pass.");
                return;
            }
        };
        let mut load = self.input_waits.assigned_per_worker();
        for fetch in unassigned {
            if !self
                .place_input_fetch(&fetch, &mut workers, &mut load)
                .await
            {
                debug!(fetch_key = %fetch.fetch_key, "no connected worker can fetch the input yet");
                if !workers.iter().any(|w| can_ever_fetch(w, &fetch)) {
                    self.announce_missing_fetch_worker(&fetch, workers.len())
                        .await;
                }
            }
        }
    }

    async fn place_input_fetch(
        &self,
        fetch: &UnassignedFetch,
        workers: &mut [WorkerInfo],
        load: &mut HashMap<String, usize>,
    ) -> bool {
        for worker in fetch_workers(workers, load, fetch)
            .into_iter()
            .filter(|w| self.job_policy.allows(w, WorkerJob::InputFetch))
        {
            let fetch_id = uuid::Uuid::now_v7().to_string();
            let signal = SessionSignal::FetchInput {
                fetch_id: fetch_id.clone(),
                project: fetch.project,
                request: fetch.request.clone(),
                needs_secrets: fetch.needs_secrets,
            };
            let claimed = self
                .call(|reply| SchedulerMsg::ClaimInputFetch {
                    worker: worker.clone(),
                    fetch_id: fetch_id.clone(),
                    signal,
                    reply,
                })
                .await
                .unwrap_or(false);
            if !claimed {
                continue;
            }

            let assigned = AssignedFetch {
                fetch_id: fetch_id.clone(),
                worker: worker.clone(),
                input_fetch_handed_to_worker_at: Instant::now(),
            };
            if !self.input_waits.assign(&fetch.fetch_key, assigned) {
                self.release_input_fetch(&worker, &fetch_id).await;
                return false;
            }
            *load.entry(worker.clone()).or_default() += 1;
            if let Some(info) = workers.iter_mut().find(|w| w.id == worker) {
                info.assigned_build_count += 1;
            }
            return true;
        }
        false
    }

    async fn announce_missing_fetch_worker(&self, fetch: &UnassignedFetch, connected: usize) {
        let (capability, wanted) = if fetch.needs_secrets {
            (EvalCapability::Secrets, "network and secrets")
        } else {
            (EvalCapability::Network, "network")
        };
        let reason =
            WaitingReason::eval_workers(capability, connected.try_into().unwrap_or(u32::MAX));
        for evaluation in self.input_waits.announce_unplaced(&fetch.fetch_key) {
            let inserted = gradient_db::status::insert_worker_message(
                &self.db.worker_db,
                evaluation,
                gradient_entity::evaluation_message::MessageLevel::Notice,
                format!("waiting for a worker with {wanted} to fetch an input"),
                "fetch".to_owned(),
            )
            .await;
            if let Err(e) = inserted {
                warn!(error = %e, %evaluation, "input wait message not recorded");
            }
            announce_worker_gap(&self.db, evaluation, fetch.project, &reason).await;
        }
    }

    async fn release_input_fetch(&self, worker: &str, fetch_id: &str) {
        let released = self
            .call(|reply| SchedulerMsg::ReleaseInputFetch {
                worker: worker.to_owned(),
                fetch_id: fetch_id.to_owned(),
                reply,
            })
            .await;
        if let Err(e) = released {
            warn!(error = %e, %worker, %fetch_id, "the build slot of an input fetch was not released");
        }
        self.build_kick.kick();
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

pub(crate) fn fetch_needs_secrets_capability(
    project: ProjectId,
    url_is_ssh: bool,
    https_git_host: Option<&str>,
    pairs: &HashSet<(ProjectId, String)>,
) -> bool {
    url_is_ssh || https_git_host.is_some_and(|host| pairs.contains(&(project, host.to_owned())))
}

fn read_store_path_from_worker_input_response(response: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(response)
        .ok()?
        .get("storePath")?
        .as_str()
        .map(str::to_owned)
}

fn can_ever_fetch(worker: &WorkerInfo, fetch: &UnassignedFetch) -> bool {
    !worker.draining
        && !fetch.excluded.contains(&worker.id)
        && worker.project_access.contains(&fetch.project)
        && WorkerCaps {
            capabilities: worker.capabilities.clone(),
            ..WorkerCaps::default()
        }
        .can_fetch_input(fetch.needs_secrets)
}

fn fetch_workers(
    workers: &[WorkerInfo],
    load: &HashMap<String, usize>,
    fetch: &UnassignedFetch,
) -> Vec<String> {
    let mut eligible: Vec<&WorkerInfo> = workers
        .iter()
        .filter(|w| can_ever_fetch(w, fetch))
        .filter(|w| w.assigned_build_count < w.max_concurrent_builds as usize)
        .collect();
    eligible.sort_by_key(|w| (load.get(&w.id).copied().unwrap_or_default(), w.id.clone()));
    eligible.into_iter().map(|w| w.id.clone()).collect()
}
