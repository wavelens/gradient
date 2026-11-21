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
use gradient_scheduler::exec_secrets::{SecretResolutionError, server_secret_values};
use gradient_scheduler::jobs::{Assignment, PendingJob};
use gradient_wire::messages::{GitServerSshHostKey, HttpsGitCredential, ServerMessage};
use gradient_wire::types::{CredentialKind, Job};

use super::store_paths::{outbound_message, project_mapper};

pub use gradient_wire::session::frame::{
    BULK_CHUNK_SIZE, HANDSHAKE_TIMEOUT, ProtoSocket, ProtoWriter, recv_client_msg, send_error,
    send_server_msg,
};

pub(super) async fn send_job_with_credentials(
    writer: &ProtoWriter,
    db: &DbContext,
    scheduler: &Scheduler,
    worker_id: &str,
    assignment: Assignment,
    assign_job: impl FnOnce(
        Job,
        Option<SecretString>,
        Vec<GitServerSshHostKey>,
        Vec<HttpsGitCredential>,
    ) -> ServerMessage,
) -> Result<bool, String> {
    let credentials = resolve_job_credentials(db, scheduler, worker_id, &assignment).await?;
    for frame in credentials.frames(|ssh_key, ssh_host_keys, https_git_credentials| {
        assign_job(
            assignment.job,
            ssh_key,
            ssh_host_keys,
            https_git_credentials,
        )
    }) {
        if send_server_msg(writer, &frame).await.is_err() {
            return Ok(false);
        }
    }
    Ok(true)
}

struct JobCredentials {
    ssh_key_and_host_keys: Option<(SecretString, Vec<GitServerSshHostKey>)>,
    https_git_credentials: Vec<HttpsGitCredential>,
    job_secrets: Vec<ServerMessage>,
}

impl JobCredentials {
    fn frames(
        self,
        assign_job: impl FnOnce(
            Option<SecretString>,
            Vec<GitServerSshHostKey>,
            Vec<HttpsGitCredential>,
        ) -> ServerMessage,
    ) -> Vec<ServerMessage> {
        let (ssh_key, ssh_host_keys) = self.ssh_key_and_host_keys.unzip();
        let mut frames = self.job_secrets;
        frames.push(assign_job(
            ssh_key,
            ssh_host_keys.unwrap_or_default(),
            self.https_git_credentials,
        ));
        frames
    }
}

async fn resolve_job_credentials(
    db: &DbContext,
    scheduler: &Scheduler,
    worker_id: &str,
    assignment: &Assignment,
) -> Result<JobCredentials, String> {
    let caps = scheduler.worker_gradient_caps(worker_id).await;
    let allows_project_credentials_and_privileged_actions = assignment
        .pending
        .allows_project_credentials_and_privileged_actions();
    let server_secrets = match (&assignment.job, &assignment.pending) {
        (Job::Exec(exec), PendingJob::Exec(pending))
            if !exec.requirement.server_secrets.is_empty() =>
        {
            Some((&exec.requirement.server_secrets, &pending.spec))
        }
        _ => None,
    };
    if server_secrets.is_some() {
        if !allows_project_credentials_and_privileged_actions {
            return Err(
                SecretResolutionError::ProjectSecretsNotAllowedForUnapprovedForkPullRequest
                    .to_string(),
            );
        }
        if !caps.as_ref().is_some_and(|c| c.secrets) {
            return Err(format!("worker {worker_id} lacks the secrets capability"));
        }
    }

    let (ssh_key_and_host_keys, https_git_credentials) =
        if should_send_project_git_credentials_to_worker(
            caps.as_ref(),
            &assignment.job,
            allows_project_credentials_and_privileged_actions,
        ) {
            (
                load_project_ssh_key_and_saved_git_server_ssh_host_keys(db, assignment.project_id)
                    .await,
                load_and_decrypt_all_project_https_git_credentials_for_worker(
                    db,
                    assignment.project_id,
                )
                .await,
            )
        } else {
            (None, Vec::new())
        };

    let job_secrets = match server_secrets {
        Some((names, spec)) => {
            let values = server_secret_values(
                &db.worker_db,
                &db.config,
                assignment.project_id,
                spec,
                names,
            )
            .await
            .map_err(|e| e.to_string())?;
            job_secret_credentials(assignment.job_id(), values)
        }
        None => Vec::new(),
    };
    Ok(JobCredentials {
        ssh_key_and_host_keys,
        https_git_credentials,
        job_secrets,
    })
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
    allows_project_credentials_and_privileged_actions: bool,
) -> bool {
    use gradient_wire::types::{FlakeStep, Job};

    allows_project_credentials_and_privileged_actions
        && caps.is_some_and(|c| c.secrets)
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
    let credentials = if sends_input_ssh_key(caps.as_ref(), &fetch, request.as_ref()) {
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
    fetch: &InputFetch,
    request: Option<&InputRequest>,
) -> bool {
    fetch.needs_secrets
        && caps.is_some_and(|c| c.secrets)
        && request.is_some_and(|r| r.needs_secrets)
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

async fn load_and_decrypt_all_project_https_git_credentials_for_worker(
    db: &DbContext,
    project_id: ProjectId,
) -> Vec<HttpsGitCredential> {
    match gradient_sources::git::load_project_https_git_credentials(db, project_id).await {
        Ok(credentials) => credentials,
        Err(e) => {
            warn!(%project_id, error = %e, "The server failed to load or decrypt the HTTPS git credentials for the project. It doesn't pass those credentials to the worker.");
            Vec::new()
        }
    }
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
            &fetch_job(),
            true
        ));
        assert!(!should_send_project_git_credentials_to_worker(
            None,
            &fetch_job(),
            true
        ));

        let with_secrets = GradientCapabilities {
            secrets: true,
            ..network_only
        };
        assert!(should_send_project_git_credentials_to_worker(
            Some(&with_secrets),
            &fetch_job(),
            true
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
            &exec_job(),
            true
        ));
        assert!(!should_send_project_git_credentials_to_worker(
            Some(&without),
            &exec_job(),
            true
        ));
        assert!(!should_send_project_git_credentials_to_worker(
            None,
            &exec_job(),
            true
        ));
    }

    #[test]
    fn untrusted_job_gets_no_project_ssh_key_even_with_secrets_capability() {
        let with_secrets = GradientCapabilities {
            exec: true,
            eval: true,
            secrets: true,
            ..Default::default()
        };
        for job in [fetch_job(), exec_job()] {
            assert!(!should_send_project_git_credentials_to_worker(
                Some(&with_secrets),
                &job,
                false
            ));
        }
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

        let fetch = |needs_secrets: bool| InputFetch {
            fetch_id: "f1".into(),
            project: ProjectId::now_v7(),
            request: String::new(),
            needs_secrets,
        };
        let (credentialed, withheld) = (fetch(true), fetch(false));

        assert!(!sends_input_ssh_key(
            Some(&network_only),
            &credentialed,
            ssh.as_ref()
        ));
        assert!(!sends_input_ssh_key(None, &credentialed, ssh.as_ref()));
        assert!(!sends_input_ssh_key(
            Some(&with_secrets),
            &credentialed,
            https.as_ref()
        ));
        assert!(!sends_input_ssh_key(
            Some(&with_secrets),
            &credentialed,
            None
        ));
        assert!(!sends_input_ssh_key(
            Some(&with_secrets),
            &withheld,
            ssh.as_ref()
        ));
        assert!(sends_input_ssh_key(
            Some(&with_secrets),
            &credentialed,
            ssh.as_ref()
        ));
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

    fn exec_assignment(
        allows_project_credentials_and_privileged_actions: bool,
        server_secrets: &[&str],
    ) -> Assignment {
        use super::super::inbound::fixture::pending_exec;
        use gradient_entity::dispatched_job::DispatchedJobKind;
        use gradient_scheduler::jobs::AssignmentRecord;
        use gradient_types::ids::{DispatchedJobId, EvaluationId, ExecJobAttemptId};

        let mut pending = pending_exec(ExecJobAttemptId::now_v7());
        if let PendingJob::Exec(exec) = &mut pending {
            exec.allows_project_credentials_and_privileged_actions =
                allows_project_credentials_and_privileged_actions;
        }
        let mut job = exec_job();
        if let Job::Exec(exec) = &mut job {
            exec.requirement.server_secrets =
                server_secrets.iter().map(|s| s.to_string()).collect();
        }
        let now = gradient_types::now();
        Assignment {
            job,
            ram_need: Default::default(),
            project_id: ProjectId::now_v7(),
            assignment_record: AssignmentRecord {
                job_id: "exec:1".into(),
                assignment_id: DispatchedJobId::now_v7(),
                kind: DispatchedJobKind::Exec,
                derivation_build: None,
                evaluation_id: EvaluationId::now_v7(),
                project: ProjectId::now_v7(),
                task: None,
                score: 0.0,
                queued_at: now,
                ready_at: now,
                score_breakdown: serde_json::Value::Null,
                worker_context: serde_json::Value::Null,
                job_context: serde_json::Value::Null,
                instance_context: serde_json::Value::Null,
                substitute: false,
                output_names: vec![],
                choice: None,
                exec: None,
            },
            pending,
        }
    }

    async fn register_worker_with_secrets(scheduler: &Scheduler) -> impl Sized {
        let (port, signals) = tokio::sync::mpsc::unbounded_channel();
        scheduler
            .register_worker(
                "w1",
                GradientCapabilities {
                    secrets: true,
                    ..Default::default()
                },
                gradient_pool::ProjectAccess::no_project_access(),
                std::sync::Arc::new(port),
                false,
            )
            .await
            .expect("the worker registers");
        signals
    }

    fn state_with_project_ssh_key(
        stored_https_credential_password: Option<&str>,
        then_secret_lookup: impl FnOnce(sea_orm::MockDatabase) -> sea_orm::MockDatabase,
    ) -> std::sync::Arc<gradient_core::ServerState> {
        let crypt_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(crypt_file.path(), b"test-secret-key-32-bytes-padding!").unwrap();
        let crypt_file = crypt_file.into_temp_path().keep().unwrap();
        let crypt_file = crypt_file.to_string_lossy().into_owned();
        let (private_key, public_key) =
            gradient_sources::ssh_key::generate_ssh_key(&crypt_file).unwrap();
        let project = MProject {
            private_key,
            public_key,
            ..gradient_fixtures::rows::project()
        };
        let https_credentials: Vec<MProjectGitHttpsCredential> = stored_https_credential_password
            .map(|password| MProjectGitHttpsCredential {
                project: project.id,
                host: "github.com".into(),
                username: "x-access-token".into(),
                password: gradient_sources::encrypt_secret(&crypt_file, password).unwrap(),
                ..Default::default()
            })
            .into_iter()
            .collect();
        let worker_db = then_secret_lookup(
            gradient_fixtures::mock::mock_db()
                .append_query_results([vec![project]])
                .append_query_results([Vec::<MProjectSshHostKey>::new()])
                .append_query_results([https_credentials]),
        )
        .into_connection();
        gradient_test_support::state::TestState::new()
            .cli(move |cli| cli.secrets.crypt_file = crypt_file)
            .worker_db(worker_db)
            .build()
    }

    #[tokio::test]
    async fn an_unresolvable_server_secret_sends_no_credential_frame() {
        use super::super::inbound::fixture::TestSession;

        let project_db =
            super::super::SessionHandles::from_state(&state_with_project_ssh_key(None, |db| {
                db.append_query_errors([sea_orm::DbErr::Custom("connection lost".into())])
            }))
            .db;
        let state = gradient_test_support::state::TestState::new().build();
        let (session, mut sent, _admitted) = TestSession::new(&state).await;
        let _signals = register_worker_with_secrets(&session.scheduler).await;

        let resolved = send_job(
            &session.writer,
            &project_db,
            &session.scheduler,
            exec_assignment(true, &["wandb-token"]),
        )
        .await;

        assert!(resolved.is_err());
        assert!(sent.try_recv().is_err());
    }

    fn assign_job(
        job: Job,
        ssh_key: Option<SecretString>,
        ssh_host_keys: Vec<GitServerSshHostKey>,
        https_git_credentials: Vec<HttpsGitCredential>,
    ) -> ServerMessage {
        ServerMessage::AssignJob {
            job_id: "exec:1".into(),
            assignment_id: "assignment-1".into(),
            job,
            cluster: None,
            ssh_key,
            ssh_host_keys,
            https_git_credentials,
        }
    }

    async fn send_job(
        writer: &ProtoWriter,
        db: &DbContext,
        scheduler: &Scheduler,
        assignment: Assignment,
    ) -> Result<bool, String> {
        send_job_with_credentials(writer, db, scheduler, "w1", assignment, assign_job).await
    }

    async fn frames_sent_for(
        assignment: Assignment,
        stored_https_credential_password: Option<&str>,
    ) -> Vec<ServerMessage> {
        use super::super::inbound::fixture::{TestSession, decode};

        let project_db = super::super::SessionHandles::from_state(&state_with_project_ssh_key(
            stored_https_credential_password,
            |db| db,
        ))
        .db;
        let state = gradient_test_support::state::TestState::new().build();
        let (session, mut sent, _admitted) = TestSession::new(&state).await;
        let _signals = register_worker_with_secrets(&session.scheduler).await;

        let sent_all = send_job(&session.writer, &project_db, &session.scheduler, assignment).await;

        assert_eq!(sent_all, Ok(true));
        std::iter::from_fn(|| sent.try_recv().ok())
            .map(decode)
            .collect()
    }

    #[tokio::test]
    async fn a_current_worker_gets_the_ssh_key_inside_assign_job_only() {
        let frames = frames_sent_for(exec_assignment(true, &[]), None).await;

        assert!(matches!(
            frames.as_slice(),
            [ServerMessage::AssignJob {
                ssh_key: Some(_),
                ..
            }]
        ));
    }

    #[tokio::test]
    async fn a_fork_job_gets_no_ssh_key_in_either_form() {
        let frames = frames_sent_for(exec_assignment(false, &[]), None).await;

        assert!(matches!(
            frames.as_slice(),
            [ServerMessage::AssignJob {
                ssh_key: None,
                ssh_host_keys,
                ..
            }] if ssh_host_keys.is_empty()
        ));
    }

    #[tokio::test]
    async fn an_unapproved_fork_job_gets_no_https_git_credential_and_no_other_frame_is_sent() {
        let frames = frames_sent_for(exec_assignment(false, &[]), Some("token")).await;

        assert!(matches!(
            frames.as_slice(),
            [ServerMessage::AssignJob {
                ssh_key: None,
                https_git_credentials,
                ..
            }] if https_git_credentials.is_empty()
        ));
    }

    #[tokio::test]
    async fn an_approved_job_sent_to_a_worker_with_secrets_capability_gets_project_https_git_credentials_inside_assign_job_as_only_frame()
     {
        let frames = frames_sent_for(exec_assignment(true, &[]), Some("token")).await;

        assert!(matches!(
            frames.as_slice(),
            [ServerMessage::AssignJob {
                ssh_key: Some(_),
                https_git_credentials,
                ..
            }] if *https_git_credentials == [HttpsGitCredential {
                host: "github.com".into(),
                username: "x-access-token".into(),
                password: "token".into(),
            }]
        ));
    }

    #[tokio::test]
    async fn send_job_with_credentials_checks_project_credentials_flag_before_sending_credential_frame()
     {
        use super::super::inbound::fixture::TestSession;
        use gradient_test_support::state::TestState;

        let assignment_for = |allows: bool| exec_assignment(allows, &["wandb-token"]);
        let state = TestState::new().build();
        let (session, mut sent, _admitted) = TestSession::new(&state).await;
        let send = async |assignment: Assignment| {
            send_job(
                &session.writer,
                &session.handles.db,
                &session.scheduler,
                assignment,
            )
            .await
        };

        assert_eq!(
            send(assignment_for(false)).await,
            Err(
                SecretResolutionError::ProjectSecretsNotAllowedForUnapprovedForkPullRequest
                    .to_string()
            )
        );
        assert!(sent.try_recv().is_err());

        assert_eq!(
            send(assignment_for(true)).await,
            Err("worker w1 lacks the secrets capability".to_owned())
        );
        assert!(sent.try_recv().is_err());

        let _signals = register_worker_with_secrets(&session.scheduler).await;
        assert!(
            session
                .scheduler
                .worker_gradient_caps("w1")
                .await
                .is_some_and(|caps| caps.secrets)
        );
        assert_eq!(
            send(assignment_for(false)).await,
            Err(
                SecretResolutionError::ProjectSecretsNotAllowedForUnapprovedForkPullRequest
                    .to_string()
            )
        );
        assert!(sent.try_recv().is_err());
    }
}
