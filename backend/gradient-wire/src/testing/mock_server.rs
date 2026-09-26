/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Mock WebSocket authority for driving a peer (a worker or a proxy) from a test.
//!
//! [`MockProtoServer`] binds `127.0.0.1:0` and accepts one connection per
//! [`MockProtoServer::accept`]. The resulting [`MockServerConn`] sends and
//! receives typed frames and scripts the authority side: handshake, job list,
//! offers, score collection, assignment and NAR relay, each wait bounded by
//! [`SCRIPT_TIMEOUT`].

use crate::constants::BULK_CHUNK_SIZE;
use crate::messages::{
    CandidateScore, ClientMessage, GradientCapabilities, Job, JobCandidate, JobKind, PROTO_VERSION,
    ServerMessage,
};
use crate::session::frame::WireMessage;
use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{WebSocketStream, accept_async};

/// A mock WebSocket server bound to a random local port.
pub struct MockProtoServer {
    listener: TcpListener,
    url: String,
}

impl MockProtoServer {
    /// Bind to `127.0.0.1:0` and return the server.
    pub async fn bind() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("failed to bind mock server");
        let port = listener.local_addr().expect("mock listener address").port();
        let url = format!("ws://127.0.0.1:{port}");
        Self { listener, url }
    }

    /// The `ws://` URL to pass to `ProtoConnection::open`.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Accept one incoming WebSocket connection.
    pub async fn accept(&self) -> MockServerConn {
        let (stream, _) = self.listener.accept().await.expect("accept failed");
        let socket = accept_async(stream)
            .await
            .expect("WebSocket handshake failed");
        MockServerConn { socket }
    }
}

/// Server-side endpoint of a mock WebSocket connection.
pub struct MockServerConn {
    socket: WebSocketStream<TcpStream>,
}

impl MockServerConn {
    /// Send a [`ServerMessage`] to the connected client.
    pub async fn send(&mut self, msg: ServerMessage) -> Result<()> {
        let bytes = msg.encode().context("failed to serialise ServerMessage")?;
        self.socket
            .send(Message::Binary(bytes))
            .await
            .context("mock server WebSocket send failed")
    }

    /// Receive the next [`ClientMessage`] from the connected client.
    /// Skips ping/pong frames transparently.
    pub async fn recv(&mut self) -> Result<ClientMessage> {
        loop {
            match self.socket.next().await {
                Some(Ok(Message::Binary(bytes))) => {
                    return ClientMessage::decode(bytes)
                        .and_then(|inbound| inbound.into_message())
                        .context("failed to deserialise ClientMessage");
                }
                Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => continue,
                Some(Ok(Message::Close(_))) => {
                    anyhow::bail!("client closed the connection")
                }
                Some(Ok(other)) => {
                    anyhow::bail!("unexpected frame: {:?}", other)
                }
                Some(Err(e)) => return Err(e.into()),
                None => anyhow::bail!("connection closed without message"),
            }
        }
    }

    /// Close the server side gracefully.
    pub async fn close(&mut self) {
        let _ = self.socket.close(None).await;
    }
}

pub const SCRIPT_TIMEOUT: Duration = Duration::from_secs(10);

/// A NAR a peer relayed to the mock, with the metadata its `NarUploaded` carried.
#[derive(Debug)]
pub struct PushedNar {
    pub job_id: String,
    pub store_path: String,
    pub compressed: Vec<u8>,
    pub nar_size: u64,
    pub nar_hash: String,
}

impl MockServerConn {
    pub async fn recv_until<T>(
        &mut self,
        mut pick: impl FnMut(ClientMessage) -> Option<T>,
    ) -> Result<T> {
        tokio::time::timeout(SCRIPT_TIMEOUT, async {
            loop {
                if let Some(picked) = pick(self.recv().await?) {
                    return Ok(picked);
                }
            }
        })
        .await
        .context("the peer never sent the message the script waits for")?
    }

    pub async fn handshake(&mut self, negotiated: GradientCapabilities) -> Result<String> {
        let id = self
            .recv_until(|msg| match msg {
                ClientMessage::InitConnection { id, .. } => Some(id),
                _ => None,
            })
            .await?;
        self.send(ServerMessage::AuthChallenge { peers: vec![] })
            .await?;
        self.recv_until(|msg| matches!(msg, ClientMessage::AuthResponse { .. }).then_some(()))
            .await?;
        self.send(ServerMessage::InitAck {
            version: PROTO_VERSION,
            capabilities: negotiated,
            authorized_peers: vec![],
            failed_peers: vec![],
        })
        .await?;
        Ok(id)
    }

    pub async fn serve_job_list(&mut self, candidates: Vec<JobCandidate>) -> Result<()> {
        self.recv_until(|msg| matches!(msg, ClientMessage::RequestJobList).then_some(()))
            .await?;
        self.send(ServerMessage::JobListChunk {
            candidates,
            is_final: true,
        })
        .await
    }

    pub async fn offer(&mut self, candidates: Vec<JobCandidate>) -> Result<()> {
        self.send(ServerMessage::JobOffer { candidates }).await
    }

    pub async fn scores(&mut self) -> Result<Vec<CandidateScore>> {
        let mut all = Vec::new();
        loop {
            let (scores, is_final) = self
                .recv_until(|msg| match msg {
                    ClientMessage::RequestJobChunk { scores, is_final } => Some((scores, is_final)),
                    _ => None,
                })
                .await?;
            all.extend(scores);
            if is_final {
                return Ok(all);
            }
        }
    }

    pub async fn job_request(&mut self) -> Result<JobKind> {
        self.recv_until(|msg| match msg {
            ClientMessage::RequestJob { kind } => Some(kind),
            _ => None,
        })
        .await
    }

    pub async fn assign(&mut self, job_id: &str, dispatch: &str, job: Job) -> Result<bool> {
        self.send(ServerMessage::AssignJob {
            job_id: job_id.into(),
            dispatch: dispatch.into(),
            job,
        })
        .await?;
        self.recv_until(|msg| match msg {
            ClientMessage::AssignJobResponse {
                job_id: answered,
                accepted,
                ..
            } if answered == job_id => Some(accepted),
            _ => None,
        })
        .await
    }

    pub async fn report(&mut self, job_id: &str) -> Result<ClientMessage> {
        self.recv_until(|msg| match &msg {
            ClientMessage::JobCompleted { job_id: done, .. }
            | ClientMessage::JobFailed { job_id: done, .. }
                if done == job_id =>
            {
                Some(msg)
            }
            _ => None,
        })
        .await
    }

    pub async fn receive_push(&mut self) -> Result<PushedNar> {
        let (job_id, store_path) = self
            .recv_until(|msg| match msg {
                ClientMessage::NarStreamHeader {
                    job_id, store_path, ..
                } => Some((job_id, store_path)),
                _ => None,
            })
            .await?;
        self.send(ServerMessage::NarPushResume {
            job_id: job_id.clone(),
            store_path: store_path.clone(),
            received_bytes: 0,
        })
        .await?;

        let mut compressed = Vec::new();
        loop {
            let (data, is_final) = self
                .recv_until(|msg| match msg {
                    ClientMessage::NarPush { data, is_final, .. } => Some((data, is_final)),
                    _ => None,
                })
                .await?;
            compressed.extend(data);
            if is_final {
                break;
            }
        }

        let (nar_size, nar_hash) = self
            .recv_until(|msg| match msg {
                ClientMessage::NarUploaded {
                    nar_size, nar_hash, ..
                } => Some((nar_size, nar_hash)),
                _ => None,
            })
            .await?;
        Ok(PushedNar {
            job_id,
            store_path,
            compressed,
            nar_size,
            nar_hash,
        })
    }

    pub async fn serve_pull(&mut self, compressed: &[u8]) -> Result<(String, String)> {
        let (job_id, store_path) = self
            .recv_until(|msg| match msg {
                ClientMessage::NarRequest { job_id, mut paths } => {
                    paths.pop().map(|path| (job_id, path))
                }
                _ => None,
            })
            .await?;
        self.send(ServerMessage::NarStreamHeader {
            job_id: job_id.clone(),
            store_path: store_path.clone(),
            total_bytes: compressed.len() as u64,
            stream_token: "mock".into(),
        })
        .await?;

        let mut offset = 0u64;
        for chunk in compressed.chunks(BULK_CHUNK_SIZE) {
            self.send(ServerMessage::NarPush {
                job_id: job_id.clone(),
                store_path: store_path.clone(),
                data: chunk.to_vec(),
                offset,
                is_final: false,
            })
            .await?;
            offset += chunk.len() as u64;
        }

        self.send(ServerMessage::NarPush {
            job_id: job_id.clone(),
            store_path: store_path.clone(),
            data: Vec::new(),
            offset,
            is_final: true,
        })
        .await?;
        Ok((job_id, store_path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::handshake::as_peer;
    use crate::traits::{CapabilitiesProvider, PeerIdentity};
    use async_trait::async_trait;

    struct Peer;

    #[async_trait]
    impl PeerIdentity for Peer {
        fn peer_id(&self) -> String {
            "peer-1".into()
        }

        async fn tokens_for(&self, _: &[String]) -> Result<Vec<(String, String)>> {
            Ok(vec![])
        }
    }

    #[async_trait]
    impl CapabilitiesProvider for Peer {
        async fn capabilities(&self) -> GradientCapabilities {
            GradientCapabilities {
                build: true,
                ..Default::default()
            }
        }
    }

    async fn connected() -> (MockServerConn, crate::session::frame::ProtoSocket) {
        let server = MockProtoServer::bind().await;
        let (conn, socket) = tokio::join!(server.accept(), crate::client::dial(server.url()));
        (conn, socket.unwrap())
    }

    #[tokio::test]
    async fn a_peer_handshakes_against_the_scripted_authority() {
        let (mut conn, mut socket) = connected().await;
        let negotiated = GradientCapabilities {
            build: true,
            ..Default::default()
        };
        let (claimed, result) = tokio::join!(
            conn.handshake(negotiated.clone()),
            as_peer(&mut socket, &Peer, &Peer)
        );
        assert_eq!(claimed.unwrap(), "peer-1");
        assert_eq!(result.unwrap().negotiated, negotiated);
    }

    #[tokio::test]
    async fn scores_accumulate_until_the_final_chunk() {
        let (mut conn, mut socket) = connected().await;
        let score = |job: &str| CandidateScore {
            job_id: job.into(),
            missing_count: 0,
            missing_nar_size: 0,
        };
        for (scores, is_final) in [(vec![score("a")], false), (vec![score("b")], true)] {
            socket
                .send_client_msg(&ClientMessage::RequestJobChunk { scores, is_final })
                .await
                .unwrap();
        }

        let ids: Vec<_> = conn
            .scores()
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.job_id)
            .collect();
        assert_eq!(ids, ["a", "b"]);
    }

    #[tokio::test]
    async fn a_relayed_push_is_resumed_from_zero_and_assembled() {
        let (mut conn, mut socket) = connected().await;
        let peer = async {
            socket
                .send_client_msg(&ClientMessage::NarStreamHeader {
                    job_id: "j".into(),
                    store_path: "/nix/store/p".into(),
                    total_bytes: None,
                    stream_token: "t".into(),
                })
                .await
                .unwrap();
            let resume = socket.recv_server_msg().await.unwrap();
            assert!(matches!(
                resume,
                ServerMessage::NarPushResume {
                    received_bytes: 0,
                    ..
                }
            ));
            for (data, offset, is_final) in [(b"ab".to_vec(), 0, false), (Vec::new(), 2, true)] {
                socket
                    .send_client_msg(&ClientMessage::NarPush {
                        job_id: "j".into(),
                        store_path: "/nix/store/p".into(),
                        data,
                        offset,
                        is_final,
                    })
                    .await
                    .unwrap();
            }

            socket
                .send_client_msg(&ClientMessage::NarUploaded {
                    job_id: "j".into(),
                    store_path: "/nix/store/p".into(),
                    file_hash: "sha256:f".into(),
                    file_size: 2,
                    nar_size: 9,
                    nar_hash: "sha256:n".into(),
                    references: vec![],
                    deriver: None,
                    ca: None,
                    multipart: None,
                })
                .await
                .unwrap();
        };
        let (pushed, ()) = tokio::join!(conn.receive_push(), peer);
        let pushed = pushed.unwrap();
        assert_eq!(pushed.compressed, b"ab");
        assert_eq!((pushed.nar_size, pushed.nar_hash.as_str()), (9, "sha256:n"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_message_that_never_comes_fails_the_script() {
        let (mut conn, _socket) = connected().await;
        assert!(conn.job_request().await.is_err());
    }
}
