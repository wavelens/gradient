/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::proto::nix_remote_client::NixRemoteClient;
use super::proto::scheduler_client::SchedulerClient;
use super::zstd_stream_compression::{ZstdStreamCompressor, ZstdStreamPieceDecompressor};
use super::*;
use crate::backend::ConnInfo;
use crate::journal::Journal;
use harmonia_protocol::daemon::FutureResultExt as _;
use harmonia_protocol::daemon::wire::{CLIENT_MAGIC, SERVER_MAGIC};
use harmonia_protocol::de::NixRead as _;
use harmonia_protocol::valid_path_info::UnkeyedValidPathInfo;
use harmonia_store_path::{StoreDir, StorePath};
use harmonia_store_path_info::NarHash;
use std::io::Cursor;
use tokio::io::AsyncBufRead;
use tonic::Code;
use tonic::transport::server::TcpIncoming;
use tonic::transport::{Channel, Server};

const ACCEPTED_BEARER_TOKEN: &str = "test-token";

fn fake_store_known_store_path() -> StorePath {
    StorePath::from_base_path("00000000000000000000000000000000-known").expect("store path")
}

fn fake_store_known_store_path_nar_bytes() -> Vec<u8> {
    (0..=u8::MAX)
        .cycle()
        .take(UNCOMPRESSED_NAR_BYTES_PER_FETCH_NARS_FRAME + 1000)
        .collect()
}

fn known_store_path_info() -> UnkeyedValidPathInfo {
    let nar = fake_store_known_store_path_nar_bytes();
    UnkeyedValidPathInfo {
        deriver: None,
        nar_hash: NarHash::digest(&nar),
        references: [fake_store_known_store_path()].into(),
        registration_time: None,
        nar_size: nar.len() as u64,
        ultimate: false,
        signatures: Default::default(),
        ca: None,
        store_dir: StoreDir::default(),
    }
}

#[derive(Debug, Clone, Copy)]
struct TestStoreWithOneStorePathAndNar;

impl HandshakeDaemonStore for TestStoreWithOneStorePathAndNar {
    type Store = Self;

    fn handshake(self) -> impl ResultLog<Output = DaemonResult<Self::Store>> + Send {
        ready(Ok(self)).empty_logs()
    }
}

impl DaemonStore for TestStoreWithOneStorePathAndNar {
    fn trust_level(&self) -> Option<TrustLevel> {
        Some(TrustLevel::Trusted)
    }

    fn query_valid_paths<'a>(
        &'a mut self,
        paths: &'a StorePathSet,
        _substitute: bool,
    ) -> impl ResultLog<Output = DaemonResult<StorePathSet>> + Send + 'a {
        let known = fake_store_known_store_path();
        ready(Ok(paths
            .iter()
            .filter(|path| **path == known)
            .cloned()
            .collect()))
        .empty_logs()
    }

    fn query_path_info<'a>(
        &'a mut self,
        path: &'a StorePath,
    ) -> impl ResultLog<Output = DaemonResult<Option<UnkeyedValidPathInfo>>> + Send + 'a {
        ready(Ok(
            (*path == fake_store_known_store_path()).then(known_store_path_info)
        ))
        .empty_logs()
    }

    fn nar_from_path<'s>(
        &'s mut self,
        _path: &'s StorePath,
    ) -> impl ResultLog<Output = DaemonResult<impl AsyncBufRead + Send + use<>>> + Send + 's {
        ready(Ok(Cursor::new(fake_store_known_store_path_nar_bytes()))).empty_logs()
    }

    fn query_path_from_hash_part<'a>(
        &'a mut self,
        hash: &'a StorePathHash,
    ) -> impl ResultLog<Output = DaemonResult<Option<StorePath>>> + Send + 'a {
        let known = fake_store_known_store_path();
        ready(Ok((known.hash() == hash).then_some(known))).empty_logs()
    }

    fn shutdown(&mut self) -> impl Future<Output = DaemonResult<()>> + Send + '_ {
        ready(Ok(()))
    }
}

struct FakeBackend(Journal);

impl Backend for FakeBackend {
    type Handler = TestStoreWithOneStorePathAndNar;

    fn journal(&self) -> &Journal {
        &self.0
    }

    fn handler(self: &Arc<Self>, _conn: ConnInfo) -> Self::Handler {
        TestStoreWithOneStorePathAndNar
    }

    fn control(&self, _: &str, _: &serde_json::Value) -> Option<anyhow::Result<serde_json::Value>> {
        None
    }
}

struct FixedBearerTokenTestCredentialChecker(Arc<FakeBackend>);

impl GrpcCallCredentialChecker for FixedBearerTokenTestCredentialChecker {
    type Backend = FakeBackend;

    async fn check_call_credentials_and_return_store_backend(
        &self,
        metadata: &MetadataMap,
        _extensions: &Extensions,
    ) -> Result<Arc<FakeBackend>, Status> {
        let expected = format!("Bearer {ACCEPTED_BEARER_TOKEN}");
        match metadata
            .get("authorization")
            .and_then(|value| value.to_str().ok())
        {
            Some(value) if value == expected => Ok(Arc::clone(&self.0)),
            _ => Err(Status::unauthenticated("bearer token")),
        }
    }
}

async fn start_nix_store_server_on_random_port_and_connect()
-> (JoinSet<Result<(), tonic::transport::Error>>, Channel) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("address");
    let service = AuthenticatedNixGrpcStoreService::new(Arc::new(
        FixedBearerTokenTestCredentialChecker(Arc::new(FakeBackend(Journal::new()))),
    ));
    let mut server = JoinSet::new();
    server.spawn(
        Server::builder()
            .add_service(service.nix_remote_grpc_service())
            .add_service(service.scheduler_grpc_service())
            .serve_with_incoming(TcpIncoming::from(listener)),
    );
    let channel = Channel::from_shared(format!("http://{address}"))
        .expect("uri")
        .connect()
        .await
        .expect("connect");
    (server, channel)
}

fn wrap_message_in_request_with_accepted_bearer_token<T>(message: T) -> Request<T> {
    let mut request = Request::new(message);
    request.metadata_mut().insert(
        "authorization",
        format!("Bearer {ACCEPTED_BEARER_TOKEN}")
            .parse()
            .expect("metadata value"),
    );
    request
}

#[tokio::test]
async fn store_info_reports_caller_is_trusted() {
    let (_server, channel) = start_nix_store_server_on_random_port_and_connect().await;
    let mut client = NixRemoteClient::new(channel);
    let reply = client
        .store_info(wrap_message_in_request_with_accepted_bearer_token(
            StoreInfoRequest {},
        ))
        .await
        .expect("store info")
        .into_inner();
    assert_eq!(reply.trusted, Some(true));
}

#[tokio::test]
async fn call_without_bearer_token_is_rejected_with_unauthenticated() {
    let (_server, channel) = start_nix_store_server_on_random_port_and_connect().await;
    let mut client = NixRemoteClient::new(channel);
    let status = client
        .store_info(StoreInfoRequest {})
        .await
        .expect_err("rejected");
    assert_eq!(status.code(), Code::Unauthenticated);
}

#[tokio::test]
async fn query_valid_paths_returns_only_paths_store_has() {
    let (_server, channel) = start_nix_store_server_on_random_port_and_connect().await;
    let mut client = NixRemoteClient::new(channel);
    let known = fake_store_known_store_path().to_string();
    let reply = client
        .query_valid_paths(wrap_message_in_request_with_accepted_bearer_token(
            QueryValidPathsRequest {
                paths: vec![
                    known.clone(),
                    "r0c60rjpjk8apx4c92k9453cj7wcaq0k-unknown".into(),
                ],
                substitute: false,
            },
        ))
        .await
        .expect("valid paths")
        .into_inner();
    assert_eq!(reply.paths, vec![known]);
}

#[tokio::test]
async fn query_path_infos_bytes_decode_to_same_path_info() {
    let (_server, channel) = start_nix_store_server_on_random_port_and_connect().await;
    let mut client = NixRemoteClient::new(channel);
    let known = fake_store_known_store_path().to_string();
    let reply = client
        .query_path_infos(wrap_message_in_request_with_accepted_bearer_token(
            QueryPathInfosRequest {
                paths: vec![
                    known.clone(),
                    "r0c60rjpjk8apx4c92k9453cj7wcaq0k-unknown".into(),
                ],
            },
        ))
        .await
        .expect("path infos")
        .into_inner();
    assert_eq!(reply.infos.len(), 1);
    assert_eq!(reply.infos[0].path, known);
    let info: UnkeyedValidPathInfo = NixReader::builder()
        .set_version(ProtocolVersion::from_parts(1, 16))
        .build_buffered(Cursor::new(reply.infos[0].info.clone()))
        .read_value()
        .await
        .expect("decode path info");
    assert_eq!(info, known_store_path_info());
}

#[tokio::test]
async fn fetch_nars_streams_zstd_frames_that_decompress_to_nar_then_empty_end_of_file_frame() {
    let (_server, channel) = start_nix_store_server_on_random_port_and_connect().await;
    let mut client = NixRemoteClient::new(channel);
    let mut frames = client
        .fetch_nars(wrap_message_in_request_with_accepted_bearer_token(
            FetchNarsRequest {
                paths: vec![fake_store_known_store_path().to_string()],
            },
        ))
        .await
        .expect("fetch nars")
        .into_inner();
    let mut decompressor = ZstdStreamPieceDecompressor::new().expect("decompressor");
    let mut nar = Vec::new();
    let mut data_frames = 0;
    loop {
        let frame = frames.message().await.expect("frame").expect("eof frame");
        assert_eq!(frame.path_index, 0);
        if frame.eof {
            assert!(frame.data.is_empty());
            break;
        }
        data_frames += 1;
        nar.extend(decompressor.decompress(&frame.data).expect("decompress"));
    }
    assert!(frames.message().await.expect("end").is_none());
    assert_eq!(data_frames, 2);
    assert_eq!(nar, fake_store_known_store_path_nar_bytes());
}

#[tokio::test]
async fn schedule_rpc_answers_cached_when_all_outputs_in_store_and_assigned_otherwise() {
    let (_server, channel) = start_nix_store_server_on_random_port_and_connect().await;
    let mut client = SchedulerClient::new(channel);
    let want = |drv_path: &str, out_key: String| ClientMsg {
        msg: Some(client_msg::Msg::Want(Want {
            drv_path: drv_path.into(),
            out_keys: vec![out_key],
            ..Want::default()
        })),
    };
    let batch = ClientMsgs {
        msgs: vec![
            want(
                "cached.drv",
                format!("{}.narinfo", fake_store_known_store_path().hash()),
            ),
            want("missing.drv", "r0c60rjpjk8apx4c92k9453cj7wcaq0k".into()),
        ],
    };
    let batches = futures::stream::iter([batch]).chain(futures::stream::pending());
    let mut answers = client
        .schedule(wrap_message_in_request_with_accepted_bearer_token(batches))
        .await
        .expect("schedule")
        .into_inner();
    let answer = answers.message().await.expect("answer").expect("batch");
    assert_eq!(
        answer.msgs,
        vec![
            SchedMsg {
                msg: Some(sched_msg::Msg::Cached(Cached {
                    drv_path: "cached.drv".into(),
                })),
            },
            SchedMsg {
                msg: Some(sched_msg::Msg::Assigned(Assigned {
                    drv_path: "missing.drv".into(),
                    worker_addr: String::new(),
                    assign_id: 2,
                    worker_system: String::new(),
                })),
            },
        ]
    );
}

#[tokio::test]
async fn test_connect_rpc_tunnels_nix_worker_protocol_and_returns_daemon_magic() {
    let (_server, channel) = start_nix_store_server_on_random_port_and_connect().await;
    let mut client = NixRemoteClient::new(channel);
    let mut compressor = ZstdStreamCompressor::new().expect("compressor");
    let hello = Chunk {
        data: compressor
            .compress(&CLIENT_MAGIC.to_le_bytes())
            .expect("compress"),
    };
    let chunks = futures::stream::iter([hello]).chain(futures::stream::pending());
    let mut replies = client
        .connect(wrap_message_in_request_with_accepted_bearer_token(chunks))
        .await
        .expect("connect")
        .into_inner();
    let mut decompressor = ZstdStreamPieceDecompressor::new().expect("decompressor");
    let mut received = Vec::new();
    while received.len() < 8 {
        let reply = replies.message().await.expect("reply").expect("chunk");
        received.extend(decompressor.decompress(&reply.data).expect("decompress"));
    }
    assert_eq!(received[..8], SERVER_MAGIC.to_le_bytes());
}
