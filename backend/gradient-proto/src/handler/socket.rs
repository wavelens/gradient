/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::DbContext;
use gradient_db::lookup::by_id;
use gradient_types::ids::ProjectId;
use gradient_types::*;
use tracing::{debug, warn};

use gradient_scheduler::Scheduler;
use gradient_wire::messages::ServerMessage;

use super::store_paths::{outbound_message, outbound_offer, project_mapper};

pub use gradient_wire::session::frame::{
    BULK_CHUNK_SIZE, HANDSHAKE_TIMEOUT, ProtoSocket, ProtoWriter, recv_client_msg, send_error,
    send_server_msg,
};
pub use gradient_wire::session::job_offer::job_offer_chunks;

pub(super) async fn push_pending_candidates(
    writer: &ProtoWriter,
    scheduler: &Scheduler,
    peer_id: &str,
) {
    let offer = scheduler.get_new_job_candidates(peer_id).await;
    let candidates = outbound_offer(offer, &scheduler.db).await;
    if candidates.is_empty() {
        return;
    }
    debug!(%peer_id, count = candidates.len(), "pushing job offer (delta) after message processing");
    for chunk in job_offer_chunks(&candidates) {
        let _ = send_server_msg(
            writer,
            &ServerMessage::JobOffer {
                candidates: chunk.to_vec(),
            },
        )
        .await;
    }
}

pub(super) async fn send_credentials_for_job(
    writer: &ProtoWriter,
    db: &DbContext,
    scheduler: &gradient_scheduler::Scheduler,
    worker_id: &str,
    job: &gradient_wire::types::Job,
    project_id: ProjectId,
) {
    let caps = scheduler.worker_gradient_caps(worker_id).await;
    if sends_ssh_key(caps.as_ref(), job) {
        send_ssh_key_credential(writer, db, project_id).await;
    }
}

fn sends_ssh_key(
    caps: Option<&gradient_wire::types::GradientCapabilities>,
    job: &gradient_wire::types::Job,
) -> bool {
    use gradient_wire::types::{FlakeStep, Job};

    caps.is_some_and(|c| c.secrets)
        && match job {
            Job::Flake(flake_job) => flake_job.steps.contains(&FlakeStep::FetchFlake),
            Job::Exec(_) => true,
            Job::Build(_) => false,
        }
}

pub(super) async fn send_input_fetch(
    writer: &ProtoWriter,
    db: &DbContext,
    scheduler: &gradient_scheduler::Scheduler,
    worker_id: &str,
    fetch: InputFetch,
) -> Result<(), gradient_wire::session::frame::SendError> {
    let Some(mapper) = project_mapper(db, fetch.project).await else {
        return Ok(());
    };
    let caps = scheduler.worker_gradient_caps(worker_id).await;
    let ssh_key = if sends_input_ssh_key(caps.as_ref(), &fetch.request) {
        project_ssh_key(db, fetch.project).await
    } else {
        None
    };
    let msg = ServerMessage::FetchInput {
        fetch_id: fetch.fetch_id,
        project: fetch.project.to_string(),
        request: fetch.request,
        ssh_key,
    };
    send_server_msg(writer, &outbound_message(msg, &mapper)).await
}

pub(super) struct InputFetch {
    pub fetch_id: String,
    pub project: ProjectId,
    pub request: String,
}

fn sends_input_ssh_key(
    caps: Option<&gradient_wire::types::GradientCapabilities>,
    request: &str,
) -> bool {
    caps.is_some_and(|c| c.secrets)
        && gradient_pool::InputRequest::parse(request).is_ok_and(|r| r.needs_secrets)
}

async fn send_ssh_key_credential(writer: &ProtoWriter, db: &DbContext, project_id: ProjectId) {
    use gradient_wire::types::CredentialKind;

    if let Some(private_key) = project_ssh_key(db, project_id).await {
        let _ = send_server_msg(
            writer,
            &ServerMessage::Credential {
                kind: CredentialKind::SshKey,
                data: private_key.into_bytes().into(),
            },
        )
        .await;
        debug!(%project_id, "SSH key credential sent");
    }
}

async fn project_ssh_key(db: &DbContext, project_id: ProjectId) -> Option<String> {
    match by_id::<EProject, _>(&db.worker_db, project_id).await {
        Ok(Some(project)) => match gradient_sources::ssh_key::decrypt_ssh_private_key(
            &db.config.secrets.crypt_file,
            project,
            &db.config.server.serve_url,
        ) {
            Ok((private_key, _public_key)) => Some(private_key),
            Err(e) => {
                debug!(%project_id, error = %e, "no SSH key for project (may be HTTPS repo)");
                None
            }
        },
        Ok(None) => {
            warn!(%project_id, "project not found for SSH key lookup");
            None
        }
        Err(e) => {
            warn!(%project_id, error = %e, "failed to fetch project for SSH key");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_wire::types::{
        ExecJob, FlakeJob, FlakeSource, FlakeStep, GradientCapabilities, Job, JobContext,
    };

    fn fetch_job() -> Job {
        Job::Flake(FlakeJob {
            steps: vec![FlakeStep::FetchFlake],
            source: FlakeSource::Repository {
                url: "git+ssh://git@example.com/repo".into(),
                commit: "abc123".into(),
            },
            wildcards: vec![],
            timeout_secs: None,
            input_overrides: vec![],
            input_update: None,
        })
    }

    #[test]
    fn no_ssh_key_without_secrets() {
        let network_only = GradientCapabilities {
            network: true,
            eval: true,
            ..Default::default()
        };
        assert!(!sends_ssh_key(Some(&network_only), &fetch_job()));
        assert!(!sends_ssh_key(None, &fetch_job()));

        let with_secrets = GradientCapabilities {
            secrets: true,
            ..network_only
        };
        assert!(sends_ssh_key(Some(&with_secrets), &fetch_job()));
    }

    fn exec_job() -> Job {
        Job::Exec(Box::new(ExecJob {
            program: "/nix/store/aaaa-check".into(),
            args: vec![],
            env: Default::default(),
            image: None,
            requirement: Default::default(),
            context: JobContext {
                repository: "git+ssh://git@example.com/repo".into(),
                commit: "abc123".into(),
                ..Default::default()
            },
            secrets: vec![],
            keep_running: false,
            timeout_secs: None,
            max_silent_secs: None,
            attempt: 1,
        }))
    }

    #[test]
    fn an_exec_job_gets_the_project_ssh_key_only_with_secrets() {
        let with_secrets = GradientCapabilities {
            exec: true,
            secrets: true,
            ..Default::default()
        };
        let without = GradientCapabilities {
            exec: true,
            ..Default::default()
        };
        assert!(sends_ssh_key(Some(&with_secrets), &exec_job()));
        assert!(!sends_ssh_key(Some(&without), &exec_job()));
        assert!(!sends_ssh_key(None, &exec_job()));
    }

    #[test]
    fn no_input_ssh_key_without_negotiated_secrets() {
        let ssh =
            r#"{"builtin":"fetchTree","args":{"type":"git","url":"git+ssh://git@example.com/r"}}"#;
        let https =
            r#"{"builtin":"fetchTree","args":{"type":"git","url":"https://example.com/r"}}"#;
        let network_only = GradientCapabilities {
            network: true,
            ..Default::default()
        };
        let with_secrets = GradientCapabilities {
            secrets: true,
            ..network_only
        };

        assert!(!sends_input_ssh_key(Some(&network_only), ssh));
        assert!(!sends_input_ssh_key(None, ssh));
        assert!(!sends_input_ssh_key(Some(&with_secrets), https));
        assert!(sends_input_ssh_key(Some(&with_secrets), ssh));
    }
}
