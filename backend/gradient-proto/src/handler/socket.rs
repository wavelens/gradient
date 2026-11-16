/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::DbContext;
use gradient_db::lookup::by_id;
use gradient_pool::InputRequest;
use gradient_types::ids::ProjectId;
use gradient_types::*;
use tracing::{debug, warn};

use gradient_scheduler::Scheduler;
use gradient_scheduler::exec_secrets::server_secret_values;
use gradient_scheduler::jobs::{Assignment, PendingJob};
use gradient_wire::messages::{GitServerSshHostKey, HttpsGitCredential, ServerMessage};
use gradient_wire::types::{CredentialKind, Job};

use super::store_paths::{outbound_message, project_mapper};

pub use gradient_wire::session::frame::{
    BULK_CHUNK_SIZE, HANDSHAKE_TIMEOUT, ProtoSocket, ProtoWriter, recv_client_msg, send_error,
    send_server_msg,
};

pub(super) async fn send_credentials_for_job(
    writer: &ProtoWriter,
    db: &DbContext,
    scheduler: &Scheduler,
    worker_id: &str,
    assignment: &Assignment,
) -> Result<(), String> {
    let caps = scheduler.worker_gradient_caps(worker_id).await;
    let job_secrets =
        load_exec_job_server_secret_credentials(db, caps.as_ref(), worker_id, assignment).await?;
    if should_send_project_git_credentials_to_worker(caps.as_ref(), &assignment.job) {
        send_ssh_key_credential(writer, db, assignment.project_id).await;
        send_project_https_git_credentials(writer, db, assignment.project_id).await;
    }
    if job_secrets.is_empty() {
        return Ok(());
    }
    let count = job_secrets.len();
    for credential in job_secrets {
        let _ = send_server_msg(writer, &credential).await;
    }
    debug!(job_id = %assignment.job_id(), count, "job secret credentials sent");
    Ok(())
}

async fn load_exec_job_server_secret_credentials(
    db: &DbContext,
    caps: Option<&gradient_wire::types::GradientCapabilities>,
    worker_id: &str,
    assignment: &Assignment,
) -> Result<Vec<ServerMessage>, String> {
    let (Job::Exec(exec), PendingJob::Exec(pending)) = (&assignment.job, &assignment.pending)
    else {
        return Ok(Vec::new());
    };
    let names = &exec.requirement.server_secrets;
    if names.is_empty() {
        return Ok(Vec::new());
    }
    if !caps.is_some_and(|c| c.secrets) {
        return Err(format!("worker {worker_id} lacks the secrets capability"));
    }

    let values = server_secret_values(
        &db.worker_db,
        &db.config,
        assignment.project_id,
        &pending.spec,
        names,
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(job_secret_credentials(assignment.job_id(), values))
}

fn job_secret_credentials(job_id: &str, values: Vec<(String, SecretBytes)>) -> Vec<ServerMessage> {
    values
        .into_iter()
        .map(|(name, value)| ServerMessage::Credential {
            kind: CredentialKind::JobSecret {
                job_id: job_id.to_owned(),
                name,
            },
            data: value,
        })
        .collect()
}

fn should_send_project_git_credentials_to_worker(
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
    let request = InputRequest::parse(&fetch.request).ok();
    let credentials = if sends_input_ssh_key(caps.as_ref(), request.as_ref()) {
        load_project_ssh_key_and_saved_git_server_ssh_host_keys(db, fetch.project).await
    } else {
        None
    };
    let (ssh_key, ssh_host_keys) = credentials.unzip();
    let https_git_credentials =
        load_https_git_credentials_for_fetch_input(db, caps.as_ref(), &fetch, request.as_ref())
            .await;
    let msg = ServerMessage::FetchInput {
        fetch_id: fetch.fetch_id,
        project: fetch.project.to_string(),
        request: fetch.request,
        ssh_key,
        ssh_host_keys: ssh_host_keys.unwrap_or_default(),
        https_git_credentials,
    };
    send_server_msg(writer, &outbound_message(msg, &mapper)).await
}

pub(super) struct InputFetch {
    pub fetch_id: String,
    pub project: ProjectId,
    pub request: String,
    pub needs_secrets: bool,
}

fn sends_input_ssh_key(
    caps: Option<&gradient_wire::types::GradientCapabilities>,
    request: Option<&InputRequest>,
) -> bool {
    caps.is_some_and(|c| c.secrets) && request.is_some_and(|r| r.needs_secrets)
}

async fn load_https_git_credentials_for_fetch_input(
    db: &DbContext,
    caps: Option<&gradient_wire::types::GradientCapabilities>,
    fetch: &InputFetch,
    request: Option<&InputRequest>,
) -> Vec<HttpsGitCredential> {
    let host = request
        .and_then(|r| r.https_git_host.as_deref())
        .filter(|_| fetch.needs_secrets && caps.is_some_and(|c| c.secrets));
    let Some(host) = host else {
        return Vec::new();
    };
    let project_id = fetch.project;
    match gradient_sources::git::load_decrypted_project_git_https_credential(db, project_id, host)
        .await
    {
        Ok(credential) => credential.into_iter().collect(),
        Err(e) => {
            warn!(%project_id, %host, error = %e, "The server doesn't pass the project's credentials for HTTPS Git to the input fetch. Loading or decrypting those credentials failed.");
            Vec::new()
        }
    }
}

async fn send_project_https_git_credentials(
    writer: &ProtoWriter,
    db: &DbContext,
    project_id: ProjectId,
) {
    let credentials = match gradient_sources::git::load_project_https_git_credentials(
        db, project_id,
    )
    .await
    {
        Ok(credentials) => credentials,
        Err(e) => {
            warn!(%project_id, error = %e, "The server failed to load or decrypt the HTTPS git credentials for the project. It doesn't pass those credentials to the worker.");
            return;
        }
    };
    for credential in credentials {
        let _ = send_server_msg(
            writer,
            &ServerMessage::Credential {
                kind: CredentialKind::GitHttpsCredential {
                    host: credential.host,
                    username: credential.username,
                },
                data: credential.password.into_bytes(),
            },
        )
        .await;
    }
}

async fn send_ssh_key_credential(writer: &ProtoWriter, db: &DbContext, project_id: ProjectId) {
    let Some((private_key, keys)) =
        load_project_ssh_key_and_saved_git_server_ssh_host_keys(db, project_id).await
    else {
        return;
    };
    let _ = send_server_msg(
        writer,
        &ServerMessage::Credential {
            kind: CredentialKind::SshKey,
            data: private_key.into_bytes(),
        },
    )
    .await;
    let _ = send_server_msg(
        writer,
        &ServerMessage::Credential {
            kind: CredentialKind::SshHostKeys { keys },
            data: SecretBytes::from(Vec::new()),
        },
    )
    .await;
    debug!(%project_id, "SSH key credential sent");
}

async fn load_project_ssh_key_and_saved_git_server_ssh_host_keys(
    db: &DbContext,
    project_id: ProjectId,
) -> Option<(SecretString, Vec<GitServerSshHostKey>)> {
    let private_key = project_ssh_key(db, project_id).await?;
    match gradient_db::project_ssh_host_keys::list_project_ssh_host_keys_for_git_connections(
        &db.worker_db,
        project_id,
    )
    .await
    {
        Ok(keys) => Some((private_key, keys)),
        Err(e) => {
            warn!(%project_id, error = %e, "The server doesn't pass the project's SSH key to the worker because loading the host keys of the project for SSH failed.");
            None
        }
    }
}

async fn project_ssh_key(db: &DbContext, project_id: ProjectId) -> Option<SecretString> {
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
        Job::Flake(Box::new(FlakeJob {
            steps: vec![FlakeStep::FetchFlake],
            source: FlakeSource::Repository {
                url: "git+ssh://git@example.com/repo".into(),
                commit: "abc123".into(),
            },
            wildcards: vec![],
            timeout_secs: None,
            input_overrides: vec![],
            input_update: None,
            flake_dir: None,
            run_overrides: None,
            nix_evaluation_fetch_url_access: Default::default(),
        }))
    }

    #[test]
    fn no_ssh_key_without_secrets() {
        let network_only = GradientCapabilities {
            network: true,
            eval: true,
            ..Default::default()
        };
        assert!(!should_send_project_git_credentials_to_worker(
            Some(&network_only),
            &fetch_job()
        ));
        assert!(!should_send_project_git_credentials_to_worker(
            None,
            &fetch_job()
        ));

        let with_secrets = GradientCapabilities {
            secrets: true,
            ..network_only
        };
        assert!(should_send_project_git_credentials_to_worker(
            Some(&with_secrets),
            &fetch_job()
        ));
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
            keep_vm_running: false,
            wait_for_terminal: false,
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
        assert!(should_send_project_git_credentials_to_worker(
            Some(&with_secrets),
            &exec_job()
        ));
        assert!(!should_send_project_git_credentials_to_worker(
            Some(&without),
            &exec_job()
        ));
        assert!(!should_send_project_git_credentials_to_worker(
            None,
            &exec_job()
        ));
    }

    #[test]
    fn each_server_secret_becomes_a_job_secret_credential() {
        let msgs = job_secret_credentials(
            "exec:1",
            vec![
                (
                    "wandb-token".into(),
                    SecretBytes::from(b"s3cr3t-value".as_slice()),
                ),
                (
                    "hf-token".into(),
                    SecretBytes::from(b"hf-value-123".as_slice()),
                ),
            ],
        );
        let kinds: Vec<_> = msgs
            .iter()
            .map(|m| match m {
                ServerMessage::Credential { kind, .. } => kind.clone(),
                _ => panic!("credential"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                CredentialKind::JobSecret {
                    job_id: "exec:1".into(),
                    name: "wandb-token".into()
                },
                CredentialKind::JobSecret {
                    job_id: "exec:1".into(),
                    name: "hf-token".into()
                },
            ]
        );
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

        let (ssh, https) = (
            InputRequest::parse(ssh).ok(),
            InputRequest::parse(https).ok(),
        );

        assert!(!sends_input_ssh_key(Some(&network_only), ssh.as_ref()));
        assert!(!sends_input_ssh_key(None, ssh.as_ref()));
        assert!(!sends_input_ssh_key(Some(&with_secrets), https.as_ref()));
        assert!(!sends_input_ssh_key(Some(&with_secrets), None));
        assert!(sends_input_ssh_key(Some(&with_secrets), ssh.as_ref()));
    }

    #[tokio::test]
    async fn input_fetch_gets_https_git_credential_only_when_needs_secrets_worker_capability_and_host_match()
     {
        let db = gradient_fixtures::database::TestDatabase::create().await;
        let conn = db.connection();
        gradient_db::rows::insert::<EUser, _>(&conn, gradient_fixtures::rows::user())
            .await
            .unwrap();
        let project =
            gradient_db::rows::insert::<EProject, _>(&conn, gradient_fixtures::rows::project())
                .await
                .unwrap()
                .id;
        let (_secret, crypt_file) = gradient_graph::testing::secret_file();
        let encrypted = gradient_sources::encrypt_secret(&crypt_file, "ghp-secret").unwrap();
        gradient_db::project_git_https_credentials::upsert(
            &conn,
            project,
            "github.com",
            "x-access-token",
            encrypted,
            None,
        )
        .await
        .unwrap();
        let state = gradient_test_support::state::TestState::new()
            .worker_db(conn)
            .cli(move |cli| cli.secrets.crypt_file = crypt_file)
            .build();
        let ctx = state.db();
        let with_secrets = GradientCapabilities {
            network: true,
            secrets: true,
            ..Default::default()
        };
        let network_only = GradientCapabilities {
            network: true,
            ..Default::default()
        };
        let credentials = async |caps: Option<&GradientCapabilities>,
                                 needs_secrets: bool,
                                 url: &str| {
            let fetch = InputFetch {
                fetch_id: "f1".into(),
                project,
                request: format!(
                    r#"{{"builtin":"fetchTree","args":{{"type":"git","url":"{url}"}}}}"#
                ),
                needs_secrets,
            };
            let request = InputRequest::parse(&fetch.request).ok();
            load_https_git_credentials_for_fetch_input(&ctx, caps, &fetch, request.as_ref()).await
        };

        assert_eq!(
            credentials(Some(&with_secrets), true, "git+https://github.com/o/r").await,
            vec![HttpsGitCredential {
                host: "github.com".into(),
                username: "x-access-token".into(),
                password: "ghp-secret".into(),
            }]
        );
        assert!(
            credentials(Some(&with_secrets), false, "https://github.com/o/r")
                .await
                .is_empty()
        );
        assert!(
            credentials(Some(&with_secrets), true, "https://gitlab.com/o/r")
                .await
                .is_empty()
        );
        assert!(
            credentials(Some(&network_only), true, "https://github.com/o/r")
                .await
                .is_empty()
        );
        assert!(
            credentials(None, true, "https://github.com/o/r")
                .await
                .is_empty()
        );
        db.finish().await;
    }
}
