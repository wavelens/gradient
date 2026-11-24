/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_scheduler::Scheduler;
use gradient_scheduler::eval_split::reply::{RETRY_AFTER_MS, ShardRequest};
use gradient_types::StorePathMapper;
use gradient_wire::messages::ServerMessage;
use gradient_wire::types::{EvalShardOutcome, EvalShardSpec};
use tracing::warn;

use super::socket::{ProtoWriter, send_server_msg};
use super::store_paths::{outbound_message, project_mapper};

pub(super) async fn handle_eval_shard_request(
    scheduler: &Scheduler,
    writer: &ProtoWriter,
    job_id: String,
    request: ShardRequest,
) {
    let outcome = match scheduler.eval_shard_request(&job_id, request).await {
        Ok(outcome) => outcome,
        Err(e) => {
            warn!(%job_id, error = %e, "eval shard request failed; asking the member to retry");
            EvalShardOutcome::Wait {
                retry_after_ms: RETRY_AFTER_MS,
            }
        }
    };

    let mapper = active_job_project_store_path_mapper(scheduler, &job_id).await;
    let reply =
        build_eval_shard_reply_with_mapped_store_paths_or_wait(job_id, outcome, mapper.as_ref());
    let _ = send_server_msg(writer, &reply).await;
}

async fn active_job_project_store_path_mapper(
    scheduler: &Scheduler,
    job_id: &str,
) -> Option<StorePathMapper> {
    let project = scheduler.active_job(job_id).await?.project_id();
    project_mapper(&scheduler.db, project).await
}

fn build_eval_shard_reply_with_mapped_store_paths_or_wait(
    job_id: String,
    outcome: EvalShardOutcome,
    mapper: Option<&StorePathMapper>,
) -> ServerMessage {
    match mapper {
        Some(mapper) => outbound_message(ServerMessage::EvalShardReply { job_id, outcome }, mapper),
        None => ServerMessage::EvalShardReply {
            job_id,
            outcome: EvalShardOutcome::Wait {
                retry_after_ms: RETRY_AFTER_MS,
            },
        },
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::{EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH, RotatedHashMapping};
    use gradient_types::ids::CacheId;
    use std::sync::Arc;

    fn eval_shard_source_outcome_with_stored_path() -> EvalShardOutcome {
        EvalShardOutcome::Source {
            store_path: format!("/nix/store/{EXAMPLE_STORED_HASH}-source"),
            fingerprint: "f".into(),
        }
    }

    #[test]
    fn eval_shard_reply_waits_without_project_mapper_and_maps_to_original_form_with_project_mapper()
    {
        let unmapped = build_eval_shard_reply_with_mapped_store_paths_or_wait(
            "j1".into(),
            eval_shard_source_outcome_with_stored_path(),
            None,
        );
        assert!(matches!(
            unmapped,
            ServerMessage::EvalShardReply {
                outcome: EvalShardOutcome::Wait {
                    retry_after_ms: RETRY_AFTER_MS
                },
                ..
            }
        ));

        let mapper = StorePathMapper::for_cache(Arc::new(RotatedHashMapping), CacheId::nil());
        let mapped = build_eval_shard_reply_with_mapped_store_paths_or_wait(
            "j1".into(),
            eval_shard_source_outcome_with_stored_path(),
            Some(&mapper),
        );
        assert!(matches!(
            mapped,
            ServerMessage::EvalShardReply {
                outcome: EvalShardOutcome::Source { store_path, .. },
                ..
            } if store_path == format!("/nix/store/{EXAMPLE_ORIGINAL_HASH}-source")
        ));
    }
}
