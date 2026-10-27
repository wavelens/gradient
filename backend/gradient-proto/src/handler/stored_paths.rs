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
use gradient_entity::RotatedHashMapping;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::shape::shape;
use gradient_graph::{Graph, testing};
use gradient_scheduler::Scheduler;
use gradient_scheduler::jobs::eval_job_key;
use gradient_test_support::state::TestState;
use gradient_types::events::EventRx;
use gradient_types::ids::{CacheId, EvaluationId};
use gradient_types::{BuildArgs, MappingContext, StoreHash, StorePathMapping};
use gradient_wire::PROTO_VERSIONS;
use gradient_wire::codec::{from_bytes, to_bytes};
use gradient_wire::messages::{ClientMessage, GradientCapabilities, ServerMessage};
use gradient_wire::types::{
    BuildOutput, CandidateScore, DerivationOutput, DiscoveredDerivation, GrantTarget, Job,
    JobCandidate, JobKind, JobUpdateKind, NarUploadMetadata, QueryMode, UploadMetadata,
    UploadObject, UploadOutcome,
};
use ractor::Actor;
use ractor::rpc::CallResult;
use tokio::net::TcpStream;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::oneshot;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use super::SessionHandles;
use super::session_actor::SessionArgs;
use super::session_actor::tests::connected_pair;
use super::sessions::{Sessions, SessionsArgs, SessionsMsg};

const A_DRV: &str = "assjydybg00pv49ff2vsw2mz7p19571x";
const A_OUT: &str = "amrz9b2yg4cd47yqcm9sjpwqlzqi15az";
const B_DRV: &str = "0z4fl7banmnkg6a4bvpa2gsa0nzqgga4";
const B_OUT: &str = "wsy3j3hj2sgar4m99y3h8y1dp9pvzyk8";
const SRC: &str = "i80q81hlgshr8896r6b1w947j8klrzfa";
const ORIGINALS: [&str; 5] = [A_DRV, A_OUT, B_DRV, B_OUT, SRC];
const WAIT: Duration = Duration::from_secs(60);

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

    async fn candidates(&mut self) -> Vec<JobCandidate> {
        self.send(ClientMessage::RequestJobList).await;
        let mut all = Vec::new();
        loop {
            let (candidates, is_final) = self
                .next(|msg| match msg {
                    ServerMessage::JobListChunk {
                        candidates,
                        is_final,
                    } => Some((candidates.clone(), *is_final)),
                    _ => None,
                })
                .await;
            all.extend(candidates);
            if is_final {
                return all;
            }
        }
    }

    async fn claim(&mut self, kind: JobKind, wanted: impl Fn(&JobCandidate) -> bool) -> Claimed {
        let deadline = tokio::time::Instant::now() + WAIT;
        while tokio::time::Instant::now() < deadline {
            let candidates = self.candidates().await;
            if let Some(candidate) = candidates.iter().find(|c| wanted(c)) {
                let scores = vec![CandidateScore {
                    job_id: candidate.job_id.clone(),
                    missing_count: 0,
                    missing_nar_size: 0,
                    outputs_present: false,
                }];
                self.send(ClientMessage::RequestJobChunk {
                    scores,
                    is_final: true,
                })
                .await;
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
                if let Some(claimed) = assigned {
                    self.send(ClientMessage::AssignJobResponse {
                        job_id: claimed.job_id.clone(),
                        accepted: true,
                        reason: None,
                    })
                    .await;
                    return claimed;
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
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
        references: Vec<String>,
        deriver: Option<String>,
    ) {
        let nar = format!("nar of {path}").into_bytes();
        self.send(ClientMessage::UploadRequest {
            job_id: claimed.job_id.clone(),
            request_id,
            object: UploadObject::Nar { store_path: path },
            size: nar.len() as u64,
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
            data: nar.clone().into(),
            offset: 0,
            is_final: true,
        })
        .await;
        self.send(ClientMessage::UploadFinished {
            request_id,
            metadata: UploadMetadata::Nar(Box::new(NarUploadMetadata {
                file_hash: gradient_storage::file_hash_sri(&nar),
                file_size: nar.len() as u64,
                nar_size: nar.len() as u64,
                nar_hash: "sha256:1111".into(),
                references,
                deriver,
                ca: None,
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
        input_sources: vec![store_path(SRC, "source")],
        architecture: "x86_64-linux".into(),
        allow_substitutes: true,
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn after_an_evaluation_build_and_upload_no_original_hash_is_stored() {
    let e1 = EvaluationId::now_v7();
    let (secret, _) = testing::secret_file();
    let db = shape()
        .evaluation(e1)
        .evaluation_status(EvaluationStatus::Queued)
        .repository("https://example.invalid/repo")
        .into_cache(CacheId::now_v7())
        .keyed(testing::signing_key(&secret.path().to_string_lossy()))
        .into_database()
        .await;
    let project = db.project_of(e1).await;
    let storage = tempfile::TempDir::new().unwrap();
    let crypt_file = secret.path().to_string_lossy().into_owned();
    let graph = Graph::new();
    let state = TestState::new()
        .worker_db(db.connection())
        .web_db(db.connection())
        .cache_db(db.connection())
        .cli(|cli| {
            cli.secrets.crypt_file = crypt_file;
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
    let args = SessionArgs {
        peer_id: "w1".into(),
        handles: SessionHandles::from_state(&state),
        scheduler: Arc::clone(&scheduler),
        socket,
        capabilities: GradientCapabilities {
            eval: true,
            build: true,
            network: true,
            ..Default::default()
        },
        authorized_peers: HashSet::from([project]),
        dialed: None,
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

    let eval = worker
        .claim(JobKind::Flake, |c| c.job_id == eval_job_key(e1, 0))
        .await;
    worker.update(&eval, JobUpdateKind::Fetching).await;
    worker
        .update(
            &eval,
            JobUpdateKind::FetchResult {
                flake_source: Some(store_path(SRC, "source")),
            },
        )
        .await;
    worker.update(&eval, JobUpdateKind::EvaluatingFlake).await;
    worker
        .update(&eval, JobUpdateKind::EvaluatingDerivations)
        .await;
    worker
        .update(
            &eval,
            JobUpdateKind::EvalResult {
                derivations: vec![
                    discovered(A_DRV, A_OUT, "a", "packages.x86_64-linux.a"),
                    discovered(B_DRV, B_OUT, "b", ""),
                ],
                warnings: Vec::new(),
                errors: Vec::new(),
            },
        )
        .await;
    let stored_a_drv = format!("{}-a.drv", rotate(A_DRV));
    eventually(&db, async |db| {
        db.find_derivation(&stored_a_drv).await.is_some()
    })
    .await;
    worker
        .upload(&eval, 1, store_path(SRC, "source"), Vec::new(), None)
        .await;
    worker
        .upload(
            &eval,
            2,
            store_path(A_DRV, "a.drv"),
            vec![format!("{SRC}-source")],
            None,
        )
        .await;
    worker
        .send(ClientMessage::CacheQuery {
            job_id: eval.job_id.clone(),
            query_id: "q1".into(),
            paths: vec![store_path(A_OUT, "a")],
            mode: QueryMode::Normal,
            nar_sizes: vec![None],
            external: false,
        })
        .await;
    worker
        .next(|msg| matches!(msg, ServerMessage::CacheStatus { .. }).then_some(()))
        .await;
    worker.complete(&eval).await;

    let original_a_drv = store_path(A_DRV, "a.drv");
    let build = worker
        .claim(JobKind::Build, |c| c.drv_paths.contains(&original_a_drv))
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
                    store_path: store_path(A_OUT, "a"),
                    hash: A_OUT.into(),
                    nar_size: Some(1),
                    nar_hash: Some("sha256:1111".into()),
                    products: Vec::new(),
                }],
                metrics: None,
                substituted: false,
            },
        )
        .await;
    worker
        .upload(
            &build,
            3,
            store_path(A_OUT, "a"),
            vec![format!("{B_OUT}-b")],
            Some(format!("{A_DRV}-a.drv")),
        )
        .await;
    worker.complete(&build).await;
    eventually(&db, async |db| db.output_stored(&stored_a_drv, "out").await).await;

    for original in ORIGINALS {
        assert_eq!(
            db.columns_containing(original).await,
            Vec::<String>::new(),
            "{original}"
        );
    }
    assert!(!db.columns_containing(&rotate(A_DRV)).await.is_empty());
    let frames = format!("{:?}", worker.received);
    for original in ORIGINALS {
        assert!(!frames.contains(&rotate(original)), "{original}");
    }
    let _ = end_flow.send(());
    let event_lines = event_lines.await.unwrap();
    assert!(!event_lines.is_empty());
    for line in &event_lines {
        for original in ORIGINALS {
            assert!(!line.contains(&rotate(original)), "{original} in {line}");
        }
    }
    let keys = object_keys(storage.path());
    assert!(!keys.is_empty());
    let root = storage.path().to_string_lossy().into_owned();
    for key in keys.iter().map(|key| key.trim_start_matches(&root)) {
        for original in ORIGINALS {
            assert!(!key.contains(original), "{original} in {key}");
        }
    }

    sessions.stop(None);
    state
        .shutdown
        .cancel_and_drain(Duration::from_secs(10))
        .await;
    db.finish().await;
}
