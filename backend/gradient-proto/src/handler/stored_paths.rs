/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use gradient_db::store_path_content_variant_trust::{
    cache_can_sign_content_variant_sql_condition, project_trusts_content_variant_exists_sql,
};
use gradient_entity::RotatedHashMapping;
use gradient_entity::evaluation::{self, EvaluationStatus};
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_entity::store_path_content_producer::{self, StoredContentVariantProducerKind};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::shape::{Shape, shape};
use gradient_graph::{Graph, testing};
use gradient_scheduler::Scheduler;
use gradient_scheduler::jobs::eval_job_key;
use gradient_test_support::content_address_and_store_path::{
    StorePathWithContentAddress, compute_derivation_store_path_and_text_content_address,
    flat_fixed_output_store_path_and_content_address_from_file_name_and_bytes,
};
use gradient_test_support::derivation_file_from_reported_facts::DerivationFileFromReportedFacts;
use gradient_test_support::state::TestState;
use gradient_types::events::EventRx;
use gradient_types::ids::{CacheId, EvaluationId, ProjectId};
use gradient_types::{BuildArgs, MappingContext, StoreHash, StorePathMapping};
use gradient_util::nar::single_file_nar;
use gradient_util::shutdown::Shutdown;
use gradient_wire::PROTO_VERSIONS;
use gradient_wire::codec::{from_bytes, to_bytes};
use gradient_wire::messages::{ClientMessage, GradientCapabilities, ServerMessage};
use gradient_wire::types::{
    BuildOutput, DerivationOutput, DiscoveredDerivation, GrantTarget, Job, JobKind, JobUpdateKind,
    NarUploadMetadata, QueryMode, UploadMetadata, UploadObject, UploadOutcome,
};
use ractor::rpc::CallResult;
use ractor::{Actor, ActorRef};
use sea_orm::ActiveEnum;
use tokio::net::TcpStream;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use super::SessionHandles;
use super::session_actor::SessionArgs;
use super::session_actor::tests::connected_pair;
use super::session_authorization::HandshakeProof;
use super::sessions::{Sessions, SessionsArgs, SessionsMsg};

const A_OUT: &str = "amrz9b2yg4cd47yqcm9sjpwqlzqi15az";
const B_DRV: &str = "0z4fl7banmnkg6a4bvpa2gsa0nzqgga4";
const B_OUT: &str = "wsy3j3hj2sgar4m99y3h8y1dp9pvzyk8";
const NIX_BASE32_HASH_PART_OF_INPUT_ADDRESSED_SOURCE_STORE_PATH: &str =
    "9w1fx1ad3jf8wnmzw5hi9dkxgs4lwxp3";
const EVALUATION_JOB_SOURCE_STORE_PATH_FILE_CONTENT: &[u8] = b"contents of the source";
const WAIT: Duration = Duration::from_secs(60);

fn source_store_path_with_content_address() -> StorePathWithContentAddress {
    flat_fixed_output_store_path_and_content_address_from_file_name_and_bytes(
        "source",
        EVALUATION_JOB_SOURCE_STORE_PATH_FILE_CONTENT,
    )
}

fn computed_source_store_path_hash_part() -> String {
    source_store_path_with_content_address().store_path["/nix/store/".len()..][..32].to_owned()
}

struct NarUploadNonHashFields {
    references: Vec<String>,
    deriver: Option<String>,
    content_address: Option<String>,
}

fn build_derivation_a_drv_file_from_reported_facts() -> DerivationFileFromReportedFacts {
    DerivationFileFromReportedFacts::from_reported_derivation_facts(&discovered(
        A_OUT,
        A_OUT,
        "a",
        "packages.x86_64-linux.a",
    ))
}

fn create_derivation_b_file() -> DerivationFileFromReportedFacts {
    DerivationFileFromReportedFacts::from_reported_derivation_facts(&discovered(
        B_DRV, B_OUT, "b", "",
    ))
}

fn hash_part_from_store_path(path: &str) -> String {
    path["/nix/store/".len()..][..32].to_owned()
}

fn all_five_original_store_path_hash_parts() -> [String; 5] {
    let a = build_derivation_a_drv_file_from_reported_facts().derivation;
    let b = create_derivation_b_file().derivation;
    [
        hash_part_from_store_path(&a.drv_path),
        hash_part_from_store_path(&a.outputs[0].path),
        hash_part_from_store_path(&b.drv_path),
        hash_part_from_store_path(&b.outputs[0].path),
        computed_source_store_path_hash_part(),
    ]
}

fn rotate(original: &str) -> String {
    RotatedHashMapping
        .inbound(
            MappingContext::Cache(CacheId::nil()),
            StoreHash::parse(original).unwrap(),
        )
        .to_string()
}

fn store_path(hash: &str, name: &str) -> String {
    format!("/nix/store/{hash}-{name}")
}

struct Worker {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    received: Vec<ServerMessage>,
}

struct Claimed {
    job_id: String,
    assignment_id: String,
    job: Job,
}

impl Worker {
    async fn send(&mut self, msg: ClientMessage) {
        let bytes = to_bytes(&msg, *PROTO_VERSIONS.end()).unwrap();
        self.socket.send(Message::Binary(bytes)).await.unwrap();
    }

    async fn next_within<T>(
        &mut self,
        limit: Duration,
        mut pick: impl FnMut(&ServerMessage) -> Option<T>,
    ) -> Option<T> {
        tokio::time::timeout(limit, async {
            loop {
                let Some(Ok(frame)) = self.socket.next().await else {
                    panic!("the server closed the session");
                };
                let Message::Binary(bytes) = frame else {
                    continue;
                };
                let msg = from_bytes::<ServerMessage>(bytes, *PROTO_VERSIONS.end()).unwrap();
                let picked = pick(&msg);
                self.received.push(msg);
                if let Some(picked) = picked {
                    return picked;
                }
            }
        })
        .await
        .ok()
    }

    async fn next<T>(&mut self, pick: impl FnMut(&ServerMessage) -> Option<T>) -> T {
        self.next_within(WAIT, pick)
            .await
            .expect("the server answers in time")
    }

    async fn claim(&mut self, kind: JobKind, wanted: impl Fn(&Claimed) -> bool) -> Claimed {
        let deadline = tokio::time::Instant::now() + WAIT;
        while tokio::time::Instant::now() < deadline {
            self.send(ClientMessage::RequestJob { kind: kind.clone() })
                .await;
            let assigned = self
                .next_within(Duration::from_secs(2), |msg| match msg {
                    ServerMessage::AssignJob {
                        job_id,
                        assignment_id,
                        job,
                        ..
                    } => Some(Claimed {
                        job_id: job_id.clone(),
                        assignment_id: assignment_id.clone(),
                        job: job.clone(),
                    }),
                    _ => None,
                })
                .await;
            let Some(claimed) = assigned else {
                continue;
            };
            let accepted = wanted(&claimed);
            self.send(ClientMessage::AssignJobResponse {
                job_id: claimed.job_id.clone(),
                accepted,
                reason: None,
                endpoint: None,
            })
            .await;
            if accepted {
                return claimed;
            }
        }
        panic!("no {kind:?} job was handed out");
    }

    async fn update(&mut self, claimed: &Claimed, update: JobUpdateKind) {
        self.send(ClientMessage::JobUpdate {
            job_id: claimed.job_id.clone(),
            assignment_id: claimed.assignment_id.clone(),
            update,
        })
        .await;
    }

    async fn complete(&mut self, claimed: &Claimed) {
        self.send(ClientMessage::JobCompleted {
            job_id: claimed.job_id.clone(),
            assignment_id: claimed.assignment_id.clone(),
            spans: Vec::new(),
            elapsed_ms: 0,
        })
        .await;
    }

    async fn upload(
        &mut self,
        claimed: &Claimed,
        request_id: u64,
        path: String,
        nar: Vec<u8>,
        stated: NarUploadNonHashFields,
    ) {
        let compressed = zstd::encode_all(&nar[..], 3).unwrap();
        self.send(ClientMessage::UploadRequest {
            job_id: claimed.job_id.clone(),
            request_id,
            object: UploadObject::Nar { store_path: path },
            size: compressed.len() as u64,
        })
        .await;
        let target = self
            .next(|msg| match msg {
                ServerMessage::UploadGrant {
                    request_id: granted,
                    target,
                } if *granted == request_id => Some(target.clone()),
                _ => None,
            })
            .await;
        assert_eq!(target, GrantTarget::Passthrough { resume_offset: 0 });
        self.send(ClientMessage::UploadChunk {
            request_id,
            data: compressed.clone().into(),
            offset: 0,
            is_final: true,
        })
        .await;
        self.send(ClientMessage::UploadFinished {
            request_id,
            metadata: UploadMetadata::Nar(Box::new(NarUploadMetadata {
                file_hash: gradient_storage::file_hash_sri(&compressed),
                file_size: compressed.len() as u64,
                nar_size: nar.len() as u64,
                nar_hash: format!(
                    "sha256:{}",
                    hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&nar))
                ),
                references: stated.references,
                deriver: stated.deriver,
                ca: stated.content_address,
                multipart: None,
            })),
        })
        .await;
        let outcome = self
            .next(|msg| match msg {
                ServerMessage::UploadCommitted {
                    request_id: committed,
                    outcome,
                } if *committed == request_id => Some(outcome.clone()),
                _ => None,
            })
            .await;
        assert_eq!(outcome, UploadOutcome::Ok);
    }
}

async fn eventually(db: &TestDatabase, ready: impl AsyncFn(&TestDatabase) -> bool) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while !ready(db).await {
        assert!(tokio::time::Instant::now() < deadline, "timed out");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn collect_event_lines(
    mut events: EventRx,
    mut flow_ended: oneshot::Receiver<()>,
) -> Vec<String> {
    let mut lines = Vec::new();
    loop {
        tokio::select! {
            biased;
            received = events.recv() => match received {
                Ok(envelope) => lines.push(envelope.to_line()),
                Err(RecvError::Lagged(missed)) => panic!("missed {missed} events"),
                Err(RecvError::Closed) => break,
            },
            _ = &mut flow_ended => break,
        }
    }
    while let Ok(envelope) = events.try_recv() {
        lines.push(envelope.to_line());
    }
    lines
}

fn object_keys(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .flat_map(|entry| {
            let path = entry.unwrap().path();
            if path.is_dir() {
                object_keys(&path)
            } else {
                vec![path.to_string_lossy().into_owned()]
            }
        })
        .collect()
}

fn discovered(drv: &str, out: &str, name: &str, attr: &str) -> DiscoveredDerivation {
    DiscoveredDerivation {
        attr: attr.into(),
        drv_path: store_path(drv, &format!("{name}.drv")),
        outputs: vec![DerivationOutput {
            name: "out".into(),
            path: store_path(out, name),
        }],
        input_sources: vec![store_path(
            &computed_source_store_path_hash_part(),
            "source",
        )],
        architecture: "x86_64-linux".into(),
        allow_substitutes: true,
        ..Default::default()
    }
}

struct RunningWorkerSessionTestSetup {
    db: TestDatabase,
    shutdown: Shutdown,
    storage: tempfile::TempDir,
    sessions: ActorRef<SessionsMsg>,
    worker: Worker,
    project: ProjectId,
    end_flow: oneshot::Sender<()>,
    event_lines: JoinHandle<Vec<String>>,
}

fn test_database_shape_with_queued_evaluation_repository_signing_cache_and_worker(
    evaluation: EvaluationId,
    signing_cache: CacheId,
    secret_path: &str,
) -> Shape {
    shape()
        .evaluation(evaluation)
        .evaluation_status(EvaluationStatus::Queued)
        .repository("https://example.invalid/repo")
        .into_cache(signing_cache)
        .keyed(testing::signing_key(secret_path))
        .worker_registration("w1")
}

async fn start_worker_session_with_capabilities_for_test_database(
    db: TestDatabase,
    evaluation: EvaluationId,
    secret_path: String,
) -> RunningWorkerSessionTestSetup {
    let project = db.project_of(evaluation).await;
    let storage = tempfile::TempDir::new().unwrap();
    let graph = Graph::new();
    let state = TestState::new()
        .worker_db(db.connection())
        .web_db(db.connection())
        .cache_db(db.connection())
        .cli(|cli| {
            cli.secrets.crypt_file = secret_path;
            cli.build = BuildArgs::default();
        })
        .nar_storage(gradient_storage::NarStore::local(storage.path()).unwrap())
        .graph(Arc::clone(&graph))
        .store_path_mapping(Arc::new(RotatedHashMapping))
        .build();
    let (end_flow, flow_ended) = oneshot::channel();
    let event_lines = state
        .shutdown
        .spawn(collect_event_lines(state.events.subscribe(), flow_ended));
    graph.spawn(state.db(), None, None).await.unwrap();
    let scheduler = Arc::new(Scheduler::new(Arc::clone(&state)));
    scheduler.start();
    let (sessions, _) = Actor::spawn(
        None,
        Sessions,
        SessionsArgs {
            scheduler: Arc::clone(&scheduler),
            shutdown: state.shutdown.clone(),
        },
    )
    .await
    .unwrap();
    let (socket, client) = connected_pair().await;
    let capabilities = GradientCapabilities {
        eval: true,
        build: true,
        network: true,
        ..Default::default()
    };
    let args = SessionArgs {
        peer_id: "w1".into(),
        handles: SessionHandles::from_state(&state),
        scheduler: Arc::clone(&scheduler),
        socket,
        advertised_capabilities: capabilities.clone(),
        capabilities,
        authorized_peers: HashSet::from([project]),
        proof: HandshakeProof::WorkerDialed {
            proven_peer_token_hashes: vec![(project.to_string(), "token-w1".into())],
        },
    };
    let CallResult::Success(Ok(_)) = sessions
        .call(|reply| SessionsMsg::Attach(Box::new(args), reply), None)
        .await
        .unwrap()
    else {
        panic!("the session attaches");
    };
    let mut worker = Worker {
        socket: client,
        received: Vec::new(),
    };
    worker
        .send(ClientMessage::WorkerCapabilities {
            architectures: vec!["x86_64-linux".into()],
            system_features: Vec::new(),
            max_concurrent_builds: 1,
            cpu_count: 4,
            ram_total_mb: 8192,
            cpu_core_score: 1,
            zone: None,
            endpoint: None,
            build_cores: None,
            exec: None,
        })
        .await;
    RunningWorkerSessionTestSetup {
        db,
        shutdown: state.shutdown.clone(),
        storage,
        sessions,
        worker,
        project,
        end_flow,
        event_lines,
    }
}

async fn claim_evaluation_job_and_upload_test_paths(
    worker: &mut Worker,
    evaluation: EvaluationId,
) -> Claimed {
    let a_derivation_file_from_reported_facts = build_derivation_a_drv_file_from_reported_facts();
    let derivation_b_drv_file_from_reported_facts = create_derivation_b_file();
    let eval = worker
        .claim(JobKind::Flake, |c| c.job_id == eval_job_key(evaluation, 0))
        .await;
    worker.update(&eval, JobUpdateKind::Fetching).await;
    worker
        .update(
            &eval,
            JobUpdateKind::FetchResult {
                flake_source: Some(store_path(
                    &computed_source_store_path_hash_part(),
                    "source",
                )),
            },
        )
        .await;
    worker.update(&eval, JobUpdateKind::EvaluatingFlake).await;
    worker
        .update(&eval, JobUpdateKind::EvaluatingDerivations)
        .await;
    worker
        .upload(
            &eval,
            1,
            store_path(&computed_source_store_path_hash_part(), "source"),
            single_file_nar(EVALUATION_JOB_SOURCE_STORE_PATH_FILE_CONTENT, false),
            NarUploadNonHashFields {
                references: Vec::new(),
                deriver: None,
                content_address: Some(source_store_path_with_content_address().content_address),
            },
        )
        .await;
    for (request_id, derivation_file) in [
        (2, &a_derivation_file_from_reported_facts),
        (3, &derivation_b_drv_file_from_reported_facts),
    ] {
        worker
            .upload(
                &eval,
                request_id,
                derivation_file.derivation.drv_path.clone(),
                derivation_file.uncompressed_nar_archive_bytes(),
                NarUploadNonHashFields {
                    references: vec![format!("{}-source", computed_source_store_path_hash_part())],
                    deriver: None,
                    content_address: None,
                },
            )
            .await;
    }
    eval
}

async fn report_result_wait_for_derivation_a_store_send_cache_query_and_complete_job(
    worker: &mut Worker,
    db: &TestDatabase,
    eval: &Claimed,
) {
    let a_derivation_file_from_reported_facts = build_derivation_a_drv_file_from_reported_facts();
    worker
        .update(
            eval,
            JobUpdateKind::EvalResult {
                derivations: vec![
                    a_derivation_file_from_reported_facts.derivation.clone(),
                    create_derivation_b_file().derivation,
                ],
                warnings: Vec::new(),
                errors: Vec::new(),
            },
        )
        .await;
    let a_drv =
        hash_part_from_store_path(&a_derivation_file_from_reported_facts.derivation.drv_path);
    let stored_a_drv = format!("{}-a.drv", rotate(&a_drv));
    eventually(db, async |db| {
        db.find_derivation(&stored_a_drv).await.is_some()
    })
    .await;
    worker
        .send(ClientMessage::CacheQuery {
            job_id: eval.job_id.clone(),
            query_id: "q1".into(),
            paths: vec![
                a_derivation_file_from_reported_facts.derivation.outputs[0]
                    .path
                    .clone(),
            ],
            mode: QueryMode::Normal,
            nar_sizes: vec![None],
            external: false,
        })
        .await;
    worker
        .next(|msg| matches!(msg, ServerMessage::CacheStatus { .. }).then_some(()))
        .await;
    worker.complete(eval).await;
}

async fn claim_build_job_for_derivation_a_as_fake_worker(worker: &mut Worker) -> Claimed {
    let a_derivation = build_derivation_a_drv_file_from_reported_facts().derivation;
    let a_output_store_path_hash = hash_part_from_store_path(&a_derivation.outputs[0].path);
    let build = worker
        .claim(
            JobKind::Build,
            |c| matches!(&c.job, Job::Build(job) if job.build.drv_path == a_derivation.drv_path),
        )
        .await;
    let Job::Build(job) = &build.job else {
        panic!("a build job, got {:?}", build.job);
    };
    let build_id = job.build.build_id.clone();
    worker
        .update(
            &build,
            JobUpdateKind::Building {
                build_id: build_id.clone(),
            },
        )
        .await;
    worker
        .update(
            &build,
            JobUpdateKind::BuildOutput {
                build_id,
                outputs: vec![BuildOutput {
                    name: "out".into(),
                    store_path: a_derivation.outputs[0].path.clone(),
                    hash: a_output_store_path_hash,
                    nar_size: Some(1),
                    nar_hash: Some("sha256:1111".into()),
                    products: Vec::new(),
                }],
                metrics: None,
                substituted: false,
            },
        )
        .await;
    build
}

async fn stop_sessions_actor_wait_for_server_shutdown_and_drop_test_database(
    setup: RunningWorkerSessionTestSetup,
) -> Vec<String> {
    let _ = setup.end_flow.send(());
    let event_lines = setup.event_lines.await.unwrap();
    setup.sessions.stop(None);
    setup
        .shutdown
        .cancel_and_drain(Duration::from_secs(10))
        .await;
    setup.db.finish().await;
    event_lines
}

#[tokio::test(flavor = "multi_thread")]
async fn after_an_evaluation_build_and_upload_no_original_hash_is_stored() {
    let a_derivation_file_from_reported_facts = build_derivation_a_drv_file_from_reported_facts();
    let derivation_b_drv_file_from_reported_facts = create_derivation_b_file();
    let source_input_store_path = store_path(&computed_source_store_path_hash_part(), "source");
    for generated_derivation_file_path in [
        &a_derivation_file_from_reported_facts,
        &derivation_b_drv_file_from_reported_facts,
    ] {
        let drv_file_name =
            &generated_derivation_file_path.derivation.drv_path["/nix/store/".len() + 33..];
        assert_eq!(
            compute_derivation_store_path_and_text_content_address(
                drv_file_name,
                &generated_derivation_file_path.text,
                std::slice::from_ref(&source_input_store_path)
            )
            .store_path,
            generated_derivation_file_path.derivation.drv_path
        );
    }
    let a_drv =
        hash_part_from_store_path(&a_derivation_file_from_reported_facts.derivation.drv_path);
    let a_output_store_path_hash = hash_part_from_store_path(
        &a_derivation_file_from_reported_facts.derivation.outputs[0].path,
    );
    let b_derivation_store_path_hash = hash_part_from_store_path(
        &derivation_b_drv_file_from_reported_facts
            .derivation
            .drv_path,
    );
    let b_output_store_path_hash = hash_part_from_store_path(
        &derivation_b_drv_file_from_reported_facts.derivation.outputs[0].path,
    );
    let originals = all_five_original_store_path_hash_parts();
    let e1 = EvaluationId::now_v7();
    let (secret, _) = testing::secret_file();
    let secret_path = secret.path().to_string_lossy().into_owned();
    let db = test_database_shape_with_queued_evaluation_repository_signing_cache_and_worker(
        e1,
        CacheId::now_v7(),
        &secret_path,
    )
    .into_database()
    .await;
    let mut setup =
        start_worker_session_with_capabilities_for_test_database(db, e1, secret_path).await;

    let eval = claim_evaluation_job_and_upload_test_paths(&mut setup.worker, e1).await;
    report_result_wait_for_derivation_a_store_send_cache_query_and_complete_job(
        &mut setup.worker,
        &setup.db,
        &eval,
    )
    .await;
    let stored_a_drv = format!("{}-a.drv", rotate(&a_drv));

    let build = claim_build_job_for_derivation_a_as_fake_worker(&mut setup.worker).await;
    setup
        .worker
        .upload(
            &build,
            4,
            store_path(&a_output_store_path_hash, "a"),
            format!("nar of a with {b_output_store_path_hash}-b").into_bytes(),
            NarUploadNonHashFields {
                references: vec![format!("{b_output_store_path_hash}-b")],
                deriver: Some(store_path(&b_derivation_store_path_hash, "b.drv")),
                content_address: None,
            },
        )
        .await;
    setup.worker.complete(&build).await;
    let db = &setup.db;
    eventually(db, async |db| db.output_stored(&stored_a_drv, "out").await).await;
    assert_eq!(
        db.cached_path(&rotate(&a_output_store_path_hash))
            .await
            .and_then(|row| row.deriver),
        Some(store_path(&rotate(&a_drv), "a.drv"))
    );

    for original in &originals {
        assert_eq!(
            db.columns_containing(original).await,
            Vec::<String>::new(),
            "{original}"
        );
    }
    assert!(!db.columns_containing(&rotate(&a_drv)).await.is_empty());
    let frames = format!("{:?}", setup.worker.received);
    for original in &originals {
        assert!(!frames.contains(&rotate(original)), "{original}");
    }
    let keys = object_keys(setup.storage.path());
    assert!(!keys.is_empty());
    let root = setup.storage.path().to_string_lossy().into_owned();
    for key in keys.iter().map(|key| key.trim_start_matches(&root)) {
        for original in &originals {
            assert!(!key.contains(original), "{original} in {key}");
        }
    }

    let event_lines =
        stop_sessions_actor_wait_for_server_shutdown_and_drop_test_database(setup).await;
    assert!(!event_lines.is_empty());
    for line in &event_lines {
        for original in &originals {
            assert!(!line.contains(&rotate(original)), "{original} in {line}");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn input_addressed_path_in_build_output_closure_but_not_listed_output_and_without_content_address_uploaded_during_build_is_committed_with_dispatched_build_job_producer_and_trusted_only_by_uploading_project()
 {
    let b_derivation = create_derivation_b_file().derivation;
    let b_output_store_path_hash = hash_part_from_store_path(&b_derivation.outputs[0].path);
    let e1 = EvaluationId::now_v7();
    let evaluation_id_of_other_project = EvaluationId::now_v7();
    let third_project_completed_evaluation_id = EvaluationId::now_v7();
    let signing_cache = CacheId::now_v7();
    let read_only_cache_id_with_signing_disabled = CacheId::now_v7();
    let other_project_cache_id = CacheId::now_v7();
    let (secret, _) = testing::secret_file();
    let secret_path = secret.path().to_string_lossy().into_owned();
    let db = test_database_shape_with_queued_evaluation_repository_signing_cache_and_worker(
        e1,
        signing_cache,
        &secret_path,
    )
    .into_cache(read_only_cache_id_with_signing_disabled)
    .keyed(testing::signing_key(&secret_path))
    .evaluation(evaluation_id_of_other_project)
    .evaluation_status(EvaluationStatus::Completed)
    .into_cache(other_project_cache_id)
    .keyed(testing::signing_key(&secret_path))
    .evaluation(third_project_completed_evaluation_id)
    .evaluation_status(EvaluationStatus::Completed)
    .into_cache(signing_cache)
    .into_database()
    .await;
    db.execute(&format!(
        "UPDATE project_cache SET mode = {}, sign_outputs = false \
         WHERE cache = '{read_only_cache_id_with_signing_disabled}'",
        CacheSubscriptionMode::ReadOnly.to_value()
    ))
    .await;
    db.execute(&format!(
        "UPDATE project_cache SET mode = {} WHERE cache = '{signing_cache}' AND project = '{}'",
        CacheSubscriptionMode::WriteOnly.to_value(),
        db.project_of(third_project_completed_evaluation_id).await
    ))
    .await;
    let mut setup =
        start_worker_session_with_capabilities_for_test_database(db, e1, secret_path).await;
    let eval = claim_evaluation_job_and_upload_test_paths(&mut setup.worker, e1).await;
    report_result_wait_for_derivation_a_store_send_cache_query_and_complete_job(
        &mut setup.worker,
        &setup.db,
        &eval,
    )
    .await;
    let build = claim_build_job_for_derivation_a_as_fake_worker(&mut setup.worker).await;

    setup
        .worker
        .upload(
            &build,
            4,
            b_derivation.outputs[0].path.clone(),
            b"nar of b".to_vec(),
            NarUploadNonHashFields {
                references: Vec::new(),
                deriver: Some(b_derivation.drv_path.clone()),
                content_address: None,
            },
        )
        .await;

    let db = &setup.db;
    let stored_cached_path_row_for_uploaded_closure_path = db
        .cached_path(&rotate(&b_output_store_path_hash))
        .await
        .expect("the uploaded closure path is stored");
    assert_eq!(
        stored_cached_path_row_for_uploaded_closure_path.deriver,
        None
    );
    let uploaded_path_store_path_content_producer_rows: Vec<_> = db
        .rows::<store_path_content_producer::Entity>()
        .await
        .into_iter()
        .filter(|row| row.cached_path == stored_cached_path_row_for_uploaded_closure_path.id)
        .collect();
    let worker_registrations = db
        .worker_registration_ids_for_project_or_insert_inactive(setup.project)
        .await;
    assert!(
        matches!(
            uploaded_path_store_path_content_producer_rows.as_slice(),
            [row] if row.kind == StoredContentVariantProducerKind::BuiltInDispatchedBuildJob
                && row.worker_registration == Some(worker_registrations[0])
        ),
        "{uploaded_path_store_path_content_producer_rows:?}"
    );
    let project_trusts_stored_content_variant = async |project: ProjectId| {
        db.texts(&format!(
            "SELECT ({})::text FROM cached_path stored WHERE stored.id = '{}'",
            project_trusts_content_variant_exists_sql("stored.id", &format!("'{project}'")),
            stored_cached_path_row_for_uploaded_closure_path.id
        ))
        .await
    };
    assert_eq!(
        project_trusts_stored_content_variant(setup.project).await,
        ["true"]
    );
    assert_eq!(
        project_trusts_stored_content_variant(db.project_of(evaluation_id_of_other_project).await)
            .await,
        ["false"]
    );
    let mut sorted_cache_ids_with_signature_for_uploaded_closure_path: Vec<CacheId> = db
        .signatures(&rotate(&b_output_store_path_hash))
        .await
        .into_iter()
        .filter(|row| row.signature.is_some())
        .map(|row| row.cache)
        .collect();
    sorted_cache_ids_with_signature_for_uploaded_closure_path.sort();
    assert_eq!(
        sorted_cache_ids_with_signature_for_uploaded_closure_path,
        [signing_cache]
    );
    let cache_may_sign_uploaded_path_stored_content_variant = async |cache: CacheId| {
        db.texts(&format!(
            "SELECT ({})::text FROM cached_path stored WHERE stored.id = '{}'",
            cache_can_sign_content_variant_sql_condition("stored.id", &format!("'{cache}'")),
            stored_cached_path_row_for_uploaded_closure_path.id
        ))
        .await
    };
    assert_eq!(
        [
            project_trusts_stored_content_variant(
                db.project_of(third_project_completed_evaluation_id).await
            )
            .await,
            cache_may_sign_uploaded_path_stored_content_variant(signing_cache).await,
            cache_may_sign_uploaded_path_stored_content_variant(
                read_only_cache_id_with_signing_disabled
            )
            .await,
            cache_may_sign_uploaded_path_stored_content_variant(other_project_cache_id).await,
        ],
        [["false"], ["true"], ["false"], ["false"]]
    );

    stop_sessions_actor_wait_for_server_shutdown_and_drop_test_database(setup).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn evaluation_worker_uploads_input_addressed_store_path_without_content_address_and_finishes_completed()
 {
    let e1 = EvaluationId::now_v7();
    let (secret, _) = testing::secret_file();
    let secret_path = secret.path().to_string_lossy().into_owned();
    let db = test_database_shape_with_queued_evaluation_repository_signing_cache_and_worker(
        e1,
        CacheId::now_v7(),
        &secret_path,
    )
    .into_database()
    .await;
    let mut setup =
        start_worker_session_with_capabilities_for_test_database(db, e1, secret_path).await;
    let eval = claim_evaluation_job_and_upload_test_paths(&mut setup.worker, e1).await;

    setup
        .worker
        .upload(
            &eval,
            4,
            store_path(
                NIX_BASE32_HASH_PART_OF_INPUT_ADDRESSED_SOURCE_STORE_PATH,
                "source",
            ),
            single_file_nar(b"an input-addressed source", false),
            NarUploadNonHashFields {
                references: Vec::new(),
                deriver: None,
                content_address: None,
            },
        )
        .await;
    report_result_wait_for_derivation_a_store_send_cache_query_and_complete_job(
        &mut setup.worker,
        &setup.db,
        &eval,
    )
    .await;
    let build = claim_build_job_for_derivation_a_as_fake_worker(&mut setup.worker).await;
    let a_output_store_path = build_derivation_a_drv_file_from_reported_facts()
        .derivation
        .outputs[0]
        .path
        .clone();
    setup
        .worker
        .upload(
            &build,
            5,
            a_output_store_path,
            b"nar of a".to_vec(),
            NarUploadNonHashFields {
                references: Vec::new(),
                deriver: None,
                content_address: None,
            },
        )
        .await;
    setup.worker.complete(&build).await;

    assert!(
        setup
            .db
            .cached_path(&rotate(
                NIX_BASE32_HASH_PART_OF_INPUT_ADDRESSED_SOURCE_STORE_PATH
            ))
            .await
            .is_some()
    );
    eventually(&setup.db, async |db| {
        db.find::<evaluation::Entity>(e1)
            .await
            .is_some_and(|row| row.status == EvaluationStatus::Completed)
    })
    .await;
    assert_eq!(setup.db.error_messages(e1).await, Vec::<String>::new());

    stop_sessions_actor_wait_for_server_shutdown_and_drop_test_database(setup).await;
}
