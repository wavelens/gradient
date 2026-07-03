/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! State-coupled helpers built on top of [`crate::session::frame`].
//!
//! Pure framing (the [`ProtoSocket`]/[`ProtoReader`]/[`ProtoWriter`]
//! abstractions and the underlying constants) moved to
//! [`crate::session::frame`]. What remains here are the helpers that need
//! access to [`ServerState`] / [`Scheduler`]: job-offer pushes and credential
//! delivery. NAR transfer moved to [`super::nar_transfer`].

use gradient_core::ServerState;
use gradient_types::ids::OrganizationId;
use gradient_types::*;
use sea_orm::EntityTrait;
use tracing::{debug, warn};

use crate::messages::ServerMessage;
use gradient_scheduler::Scheduler;

pub use crate::session::frame::{
    HANDSHAKE_TIMEOUT, JOB_OFFER_CHUNK_SIZE, NAR_PUSH_CHUNK_SIZE, ProtoSocket, ProtoWriter,
    recv_client_msg, send_error, send_server_msg,
};

/// Push any pending job candidates to the worker (delta).
pub(super) async fn push_pending_candidates(
    writer: &ProtoWriter,
    scheduler: &Scheduler,
    peer_id: &str,
) {
    let candidates = scheduler.get_new_job_candidates(peer_id).await;
    if candidates.is_empty() {
        return;
    }
    debug!(%peer_id, count = candidates.len(), "pushing job offer (delta) after message processing");
    for chunk in candidates.chunks(JOB_OFFER_CHUNK_SIZE) {
        let _ = send_server_msg(
            writer,
            &ServerMessage::JobOffer {
                candidates: chunk.to_vec(),
            },
        )
        .await;
    }
}

// ── Credential delivery ───────────────────────────────────────────────────────

pub(super) async fn send_credentials_for_job(
    writer: &ProtoWriter,
    state: &ServerState,
    scheduler: &gradient_scheduler::Scheduler,
    worker_id: &str,
    job: &gradient_types::proto::Job,
    org_id: OrganizationId,
) {
    use gradient_types::proto::{FlakeTask, Job};

    let caps = scheduler.worker_gradient_caps(worker_id).await;
    let worker_can_fetch = caps.as_ref().map(|c| c.fetch).unwrap_or(false);

    match job {
        Job::Flake(flake_job) => {
            if worker_can_fetch && flake_job.tasks.contains(&FlakeTask::FetchFlake) {
                send_ssh_key_credential(writer, state, org_id).await;
            }
        }
        Job::Build(_) => {}
    }
}

async fn send_ssh_key_credential(
    writer: &ProtoWriter,
    state: &ServerState,
    org_id: OrganizationId,
) {
    use gradient_types::proto::CredentialKind;

    match EOrganization::find_by_id(org_id)
        .one(&state.worker_db)
        .await
    {
        Ok(Some(org)) => {
            match gradient_sources::ssh_key::decrypt_ssh_private_key(
                &state.config.secrets.crypt_secret_file,
                org,
                &state.config.server.serve_url,
            ) {
                Ok((private_key, _public_key)) => {
                    let _ = send_server_msg(
                        writer,
                        &ServerMessage::Credential {
                            kind: CredentialKind::SshKey,
                            data: private_key.into_bytes(),
                        },
                    )
                    .await;
                    debug!(%org_id, "SSH key credential sent");
                }
                Err(e) => {
                    debug!(%org_id, error = %e, "no SSH key for org (may be HTTPS repo)");
                }
            }
        }
        Ok(None) => warn!(%org_id, "org not found for SSH key lookup"),
        Err(e) => warn!(%org_id, error = %e, "failed to fetch org for SSH key"),
    }
}
