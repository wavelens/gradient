/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_scheduler::Scheduler;
use gradient_scheduler::eval_split::reply::RETRY_AFTER_MS;
use gradient_wire::messages::ServerMessage;
use gradient_wire::types::{EvalShardOutcome, EvalShardSpec, FinishedShard};
use tracing::warn;

use super::socket::{ProtoWriter, send_server_msg};
use super::store_paths::{outbound_message, project_mapper};

pub(super) async fn handle_eval_shard_request(
    scheduler: &Scheduler,
    writer: &ProtoWriter,
    job_id: String,
    have_source: bool,
    finished: Option<FinishedShard>,
) {
    let outcome = match scheduler
        .eval_shard_request(&job_id, have_source, finished)
        .await
    {
        Ok(outcome) => outcome,
        Err(e) => {
            warn!(%job_id, error = %e, "eval shard request failed; asking the member to retry");
            EvalShardOutcome::Wait {
                retry_after_ms: RETRY_AFTER_MS,
            }
        }
    };

    let reply = ServerMessage::EvalShardReply {
        job_id: job_id.clone(),
        outcome,
    };
    let project = scheduler
        .active_job(&job_id)
        .await
        .map(|job| job.project_id());
    let reply = match project {
        Some(project) => match project_mapper(&scheduler.db, project).await {
            Some(mapper) => outbound_message(reply, &mapper),
            None => reply,
        },
        None => reply,
    };
    let _ = send_server_msg(writer, &reply).await;
}

pub(super) async fn send_reply(writer: &ProtoWriter, job_id: String, outcome: EvalShardOutcome) {
    let _ = send_server_msg(writer, &ServerMessage::EvalShardReply { job_id, outcome }).await;
}

pub(super) async fn handle_eval_source_ready(
    scheduler: &Scheduler,
    worker_id: &str,
    job_id: String,
    store_path: String,
    fingerprint: String,
) {
    if let Err(e) = scheduler
        .eval_source_ready(worker_id, &job_id, store_path, fingerprint)
        .await
    {
        warn!(%job_id, error = %e, "recording the split evaluation source failed");
    }
}

pub(super) async fn handle_eval_shards_planned(
    scheduler: &Scheduler,
    job_id: String,
    shards: Vec<EvalShardSpec>,
) {
    if let Err(e) = scheduler.record_eval_plan(&job_id, shards).await {
        warn!(%job_id, error = %e, "recording the split evaluation plan failed");
    }
}
