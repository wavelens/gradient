/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_wire::messages::{ClientMessage, ServerMessage};
use gradient_wire::types::{EvalCachePullOutcome, UploadOutcome};
use tracing::warn;

use super::socket::{ProtoWriter, send_server_msg};

pub(super) fn refusal(msg: &ClientMessage, reason: &str) -> Vec<ServerMessage> {
    let message = format!("{} refused: {reason}", msg.variant_name());
    match msg {
        ClientMessage::CacheQuery { query_id, .. }
        | ClientMessage::QueryKnownDerivations { query_id, .. } => {
            vec![ServerMessage::CacheError {
                query_id: query_id.clone(),
                message,
            }]
        }
        ClientMessage::NarRequest { job_id, paths } => paths
            .iter()
            .map(|store_path| nar_unavailable(job_id, store_path, &message))
            .collect(),
        ClientMessage::NarRequestResume {
            job_id, store_path, ..
        } => vec![nar_unavailable(job_id, store_path, &message)],
        ClientMessage::UploadRequest { request_id, .. }
        | ClientMessage::UploadFinished { request_id, .. } => {
            vec![ServerMessage::UploadCommitted {
                request_id: *request_id,
                outcome: UploadOutcome::Rejected { reason: message },
            }]
        }
        ClientMessage::EvalCachePull { job_id, .. } => vec![ServerMessage::EvalCachePullResult {
            job_id: job_id.clone(),
            outcome: EvalCachePullOutcome::Miss,
        }],
        _ => vec![ServerMessage::Error { code: 400, message }],
    }
}

pub(super) async fn refuse(writer: &ProtoWriter, msg: &ClientMessage, reason: &str) {
    warn!(variant = msg.variant_name(), job_id = msg.job_id(), %reason, "message refused");
    for reply in refusal(msg, reason) {
        if send_server_msg(writer, &reply).await.is_err() {
            return;
        }
    }
}

fn nar_unavailable(job_id: &str, store_path: &str, reason: &str) -> ServerMessage {
    ServerMessage::NarUnavailable {
        job_id: job_id.to_owned(),
        store_path: store_path.to_owned(),
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nar_request_is_refused_path_by_path() {
        let msg = ClientMessage::NarRequest {
            job_id: "j1".into(),
            paths: vec!["/nix/store/a".into(), "/nix/store/b".into()],
        };
        let refused: Vec<String> = refusal(&msg, "no job")
            .into_iter()
            .map(|reply| match reply {
                ServerMessage::NarUnavailable { store_path, .. } => store_path,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(refused, vec!["/nix/store/a", "/nix/store/b"]);
    }

    #[test]
    fn each_request_is_refused_in_the_reply_its_sender_waits_for() {
        let pull = ClientMessage::EvalCachePull {
            job_id: "j1".into(),
            fingerprint: "f".into(),
        };
        assert!(matches!(
            refusal(&pull, "no job").as_slice(),
            [ServerMessage::EvalCachePullResult {
                outcome: EvalCachePullOutcome::Miss,
                ..
            }]
        ));

        let finished = ClientMessage::UploadFinished {
            request_id: 7,
            metadata: gradient_wire::types::UploadMetadata::EvalCache { size_bytes: 1 },
        };
        assert!(matches!(
            refusal(&finished, "no job").as_slice(),
            [ServerMessage::UploadCommitted {
                request_id: 7,
                outcome: UploadOutcome::Rejected { .. }
            }]
        ));

        let import = ClientMessage::ImportRequest {
            job_id: "j1".into(),
            request_id: "r".into(),
            drv_paths: vec![],
        };
        assert!(matches!(
            refusal(&import, "no job").as_slice(),
            [ServerMessage::Error { code: 400, .. }]
        ));
    }
}
