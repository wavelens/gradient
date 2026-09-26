/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! A scripted worker on the real client connection, for driving an authority
//! (the server or a proxy) from a test.

use anyhow::{Context, Result};
use gradient_wire::messages::{
    BuildFailureKind, CandidateScore, ClientMessage, GradientCapabilities, Job, JobCandidate,
    JobKind, ServerMessage,
};
use gradient_wire::session::frame::Inbound;
use gradient_wire::session::handshake::HandshakeResult;
use gradient_wire::testing::SCRIPT_TIMEOUT;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::connection::handshake::perform_handshake;
use crate::connection::{ProtoConnection, ProtoWriter};
use crate::nar::{NarSink, NarSource, upload_nar};
use crate::nar_recv::NarReceiver;

pub struct PeerSpec {
    pub id: String,
    pub tokens: Vec<(String, String)>,
    pub capabilities: GradientCapabilities,
    pub architectures: Vec<String>,
    pub system_features: Vec<String>,
    pub max_concurrent_builds: u32,
}

impl Default for PeerSpec {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            tokens: vec![],
            capabilities: GradientCapabilities {
                build: true,
                ..Default::default()
            },
            architectures: vec!["x86_64-linux".into()],
            system_features: vec![],
            max_concurrent_builds: 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Assignment {
    pub job_id: String,
    pub dispatch: String,
    pub job: Job,
}

pub struct ProtoPeer {
    pub handshake: HandshakeResult,
    writer: ProtoWriter,
    inbox: mpsc::UnboundedReceiver<ServerMessage>,
    nar_recv: NarReceiver,
    pump: JoinHandle<()>,
}

impl Drop for ProtoPeer {
    fn drop(&mut self) {
        self.pump.abort();
    }
}

impl ProtoPeer {
    pub async fn connect(url: &str, spec: PeerSpec) -> Result<Self> {
        let mut conn = ProtoConnection::open(url).await?;
        let handshake = tokio::time::timeout(
            SCRIPT_TIMEOUT,
            perform_handshake(&mut conn, spec.id, spec.tokens, spec.capabilities),
        )
        .await
        .context("the authority never finished the handshake")??;
        conn.set_server_version(handshake.server_version);
        let (writer, mut reader, _flush) = conn.split();
        let nar_recv = NarReceiver::new();
        let (tx, inbox) = mpsc::unbounded_channel();
        let routed = nar_recv.clone();

        #[expect(
            clippy::disallowed_methods,
            reason = "test harness, no shutdown tracker"
        )]
        let pump = tokio::spawn(async move {
            while let Some(inbound) = reader.recv().await {
                if let Some(Inbound::Control(msg)) = routed.absorb(inbound).await
                    && tx.send(msg).is_err()
                {
                    break;
                }
            }
        });

        let peer = Self {
            handshake,
            writer,
            inbox,
            nar_recv,
            pump,
        };
        if peer.handshake.negotiated.build {
            peer.send(ClientMessage::WorkerCapabilities {
                architectures: spec.architectures,
                system_features: spec.system_features,
                max_concurrent_builds: spec.max_concurrent_builds,
                cpu_count: 1,
                ram_total_mb: 1024,
                cpu_core_score: 1,
            })
            .await?;
        }

        Ok(peer)
    }

    pub fn id(&self) -> &str {
        &self.handshake.peer_id
    }

    pub async fn send(&self, msg: ClientMessage) -> Result<()> {
        self.writer.send(msg).await
    }

    /// Skip messages until `pick` accepts one, failing after [`SCRIPT_TIMEOUT`].
    pub async fn recv_until<T>(
        &mut self,
        mut pick: impl FnMut(ServerMessage) -> Option<T>,
    ) -> Result<T> {
        tokio::time::timeout(SCRIPT_TIMEOUT, async {
            loop {
                let msg = self.inbox.recv().await.context("the authority closed")?;
                if let Some(picked) = pick(msg) {
                    return Ok(picked);
                }
            }
        })
        .await
        .context("the authority never sent the message the script waits for")?
    }

    pub async fn candidates(&mut self) -> Result<Vec<JobCandidate>> {
        self.send(ClientMessage::RequestJobList).await?;
        let mut all = Vec::new();
        loop {
            let (candidates, is_final) = self
                .recv_until(|msg| match msg {
                    ServerMessage::JobListChunk {
                        candidates,
                        is_final,
                    } => Some((candidates, is_final)),
                    _ => None,
                })
                .await?;
            all.extend(candidates);
            if is_final {
                return Ok(all);
            }
        }
    }

    pub async fn score(&self, scores: Vec<CandidateScore>) -> Result<()> {
        self.send(ClientMessage::RequestJobChunk {
            scores,
            is_final: true,
        })
        .await
    }

    pub async fn claim(&mut self, kind: JobKind) -> Result<Assignment> {
        self.send(ClientMessage::RequestJob { kind }).await?;
        let assignment = self
            .recv_until(|msg| match msg {
                ServerMessage::AssignJob {
                    job_id,
                    dispatch,
                    job,
                } => Some(Assignment {
                    job_id,
                    dispatch,
                    job,
                }),
                _ => None,
            })
            .await?;
        self.send(ClientMessage::AssignJobResponse {
            job_id: assignment.job_id.clone(),
            accepted: true,
            reason: None,
        })
        .await?;
        Ok(assignment)
    }

    pub async fn complete(&self, assignment: &Assignment) -> Result<()> {
        self.send(ClientMessage::JobCompleted {
            job_id: assignment.job_id.clone(),
            dispatch: assignment.dispatch.clone(),
            spans: vec![],
        })
        .await
    }

    pub async fn fail(&self, assignment: &Assignment, kind: BuildFailureKind) -> Result<()> {
        self.send(ClientMessage::JobFailed {
            job_id: assignment.job_id.clone(),
            dispatch: assignment.dispatch.clone(),
            error: "scripted failure".into(),
            kind,
            missing_paths: vec![],
            spans: vec![],
        })
        .await
    }

    pub async fn push_nar(&self, job_id: &str, store_path: &str, nar: Vec<u8>) -> Result<()> {
        let source = NarSource::Raw {
            nar,
            references: vec![],
            deriver: None,
            ca: None,
        };
        let sink = NarSink::Relay {
            nar_recv: &self.nar_recv,
        };
        upload_nar(job_id, store_path, source, sink, &self.writer).await
    }

    pub async fn pull_nar(&self, job_id: &str, store_path: &str) -> Result<Vec<u8>> {
        let pending = self.nar_recv.register(job_id, store_path);
        self.send(ClientMessage::NarRequest {
            job_id: job_id.into(),
            paths: vec![store_path.into()],
        })
        .await?;
        let payload = tokio::time::timeout(SCRIPT_TIMEOUT, self.nar_recv.await_pending(pending))
            .await
            .context("the authority never served the NAR")??;
        Ok(payload.read_bytes().await?.into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_wire::messages::{BuildJob, RequiredPath};
    use gradient_wire::testing::{MockProtoServer, MockServerConn};

    const PATH: &str = "/nix/store/0c0f6hb8s2q2wbdrhl5ch9fwr8v2rq7x-peer";

    fn build_caps() -> GradientCapabilities {
        GradientCapabilities {
            build: true,
            ..Default::default()
        }
    }

    async fn pair(spec: PeerSpec) -> (MockServerConn, ProtoPeer) {
        let server = MockProtoServer::bind().await;
        let (conn, peer) = tokio::join!(
            async {
                let mut conn = server.accept().await;
                conn.handshake(build_caps()).await.unwrap();
                conn
            },
            ProtoPeer::connect(server.url(), spec)
        );
        (conn, peer.unwrap())
    }

    #[tokio::test]
    async fn connecting_advertises_the_spec() {
        let spec = PeerSpec {
            architectures: vec!["aarch64-linux".into()],
            max_concurrent_builds: 3,
            ..Default::default()
        };
        let id = spec.id.clone();
        let (mut conn, peer) = pair(spec).await;
        assert_eq!(peer.id(), id);

        let (archs, slots) = conn
            .recv_until(|msg| match msg {
                ClientMessage::WorkerCapabilities {
                    architectures,
                    max_concurrent_builds,
                    ..
                } => Some((architectures, max_concurrent_builds)),
                _ => None,
            })
            .await
            .unwrap();
        assert_eq!((archs, slots), (vec!["aarch64-linux".to_owned()], 3));
    }

    #[tokio::test]
    async fn a_job_goes_offer_score_claim_complete() {
        let (mut conn, mut peer) = pair(PeerSpec::default()).await;
        let candidate = JobCandidate {
            job_id: "build:1".into(),
            required_paths: vec![RequiredPath {
                path: PATH.into(),
                cache_info: None,
            }],
            drv_paths: vec![],
        };

        let (served, seen) = tokio::join!(
            conn.serve_job_list(vec![candidate.clone()]),
            peer.candidates()
        );
        served.unwrap();
        assert_eq!(seen.unwrap(), vec![candidate]);

        let score = CandidateScore {
            job_id: "build:1".into(),
            missing_count: 1,
            missing_nar_size: 42,
        };
        let (sent, scores) = tokio::join!(peer.score(vec![score.clone()]), conn.scores());
        sent.unwrap();
        assert_eq!(scores.unwrap(), vec![score]);

        let job = Job::Build(BuildJob { builds: vec![] });
        let server_side = async {
            assert_eq!(conn.job_request().await.unwrap(), JobKind::Build);
            assert!(conn.assign("build:1", "d-1", job.clone()).await.unwrap());
            conn.report("build:1").await.unwrap()
        };
        let peer_side = async {
            let assignment = peer.claim(JobKind::Build).await.unwrap();
            assert_eq!(assignment.dispatch, "d-1");
            peer.complete(&assignment).await.unwrap();
        };
        let (report, ()) = tokio::join!(server_side, peer_side);
        assert!(matches!(
            report,
            ClientMessage::JobCompleted { dispatch, .. } if dispatch == "d-1"
        ));
    }

    #[tokio::test]
    async fn a_pushed_nar_arrives_compressed_with_its_metadata() {
        let (mut conn, peer) = pair(PeerSpec::default()).await;
        let nar = b"nix-archive-1 peer test".to_vec();
        let (pushed, sent) = tokio::join!(
            conn.receive_push(),
            peer.push_nar("build:1", PATH, nar.clone())
        );
        sent.unwrap();
        let pushed = pushed.unwrap();
        assert_eq!(pushed.store_path, PATH);
        assert_eq!(pushed.nar_size, nar.len() as u64);
        assert_eq!(zstd::decode_all(pushed.compressed.as_slice()).unwrap(), nar);
    }

    #[tokio::test]
    async fn a_pulled_nar_is_the_object_the_authority_sent() {
        let (mut conn, peer) = pair(PeerSpec::default()).await;
        let object = vec![7u8; gradient_wire::constants::BULK_CHUNK_SIZE + 5];
        let (served, pulled) =
            tokio::join!(conn.serve_pull(&object), peer.pull_nar("build:1", PATH));
        assert_eq!(served.unwrap(), ("build:1".to_owned(), PATH.to_owned()));
        assert_eq!(pulled.unwrap(), object);
    }

    #[tokio::test(start_paused = true)]
    async fn an_authority_that_never_answers_the_handshake_fails_the_connect() {
        let server = MockProtoServer::bind().await;
        let (_conn, peer) = tokio::join!(
            server.accept(),
            ProtoPeer::connect(server.url(), PeerSpec::default())
        );
        assert!(peer.is_err());
    }

    #[tokio::test]
    async fn a_pull_the_authority_ignores_fails_the_script() {
        let (_conn, peer) = pair(PeerSpec::default()).await;
        tokio::time::pause();
        assert!(peer.pull_nar("build:1", PATH).await.is_err());
    }
}
