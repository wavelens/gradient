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
use bytes::Bytes;
use harmonia_protocol::daemon::FutureResultExt as _;
use harmonia_protocol::daemon::wire::types2::QueryMissingResult;
use harmonia_protocol::daemon::wire::{CLIENT_MAGIC, SERVER_MAGIC};
use harmonia_protocol::de::NixRead as _;
use harmonia_protocol::valid_path_info::UnkeyedValidPathInfo;
use harmonia_store_derivation::derived_path::{DerivedPath, SingleDerivedPath};
use harmonia_store_path::{StoreDir, StorePath};
use harmonia_store_path_info::NarHash;
use std::io::Cursor;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::AsyncBufRead;
use tonic::Code;
use tonic::codegen::{Service as _, http};
use tonic::transport::server::TcpIncoming;
use tonic::transport::{Channel, Server};
use zstd::zstd_safe::DCtx;

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

fn fake_store_known_build_log_drv_store_path() -> StorePath {
    StorePath::from_base_path("vvpfrjjb83gs895v1881287a9pzy94nw-hello.drv").expect("store path")
}

fn fake_store_known_derivation_build_log_bytes() -> Vec<u8> {
    b"building '/nix/store/vvpfrjjb83gs895v1881287a9pzy94nw-hello.drv'\n"
        .repeat(UNCOMPRESSED_BUILD_LOG_BYTES_PER_GET_BUILD_LOG_CHUNK / 16)
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

    fn query_missing<'a>(
        &'a mut self,
        paths: &'a [DerivedPath],
    ) -> impl ResultLog<Output = DaemonResult<QueryMissingResult>> + Send + 'a {
        let known = fake_store_known_store_path();
        let mut missing = QueryMissingResult {
            will_build: StorePathSet::new(),
            will_substitute: StorePathSet::new(),
            unknown: StorePathSet::new(),
            download_size: 0,
            nar_size: 0,
        };
        for path in paths {
            match path {
                DerivedPath::Built { drv_path, .. } => {
                    if let SingleDerivedPath::Opaque(drv) = drv_path.as_ref() {
                        missing.will_build.insert(drv.clone());
                    }
                }
                DerivedPath::Opaque(path) if *path != known => {
                    missing.unknown.insert(path.clone());
                }
                DerivedPath::Opaque(_) => {}
            }
        }
        ready(Ok(missing)).empty_logs()
    }

    fn shutdown(&mut self) -> impl Future<Output = DaemonResult<()>> + Send + '_ {
        ready(Ok(()))
    }
}

impl StoreFeaturesBeyondDaemonProtocol for TestStoreWithOneStorePathAndNar {
    async fn read_build_log(&mut self, drv_path: &StorePath) -> DaemonResult<Option<Bytes>> {
        Ok((*drv_path == fake_store_known_build_log_drv_store_path())
            .then(|| Bytes::from(fake_store_known_derivation_build_log_bytes())))
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

    type OpenCallSlotPermit = ();

    async fn check_call_credentials_and_return_store_backend(
        &self,
        headers: &HeaderMap,
        _extensions: &Extensions,
    ) -> Result<AuthenticatedGrpcCallWithOpenCallSlotGuard<FakeBackend, ()>, Status> {
        let expected = format!("Bearer {ACCEPTED_BEARER_TOKEN}");
        match headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
        {
            Some(value) if value == expected => Ok(AuthenticatedGrpcCallWithOpenCallSlotGuard {
                project_nix_store_backend: Arc::clone(&self.0),
                open_call_slot_permit: (),
                caller_lost_access_future: Box::pin(futures::future::pending()),
            }),
            _ => Err(Status::unauthenticated("bearer token")),
        }
    }
}

struct FixedBearerTokenOneCallSlotTestCredentialChecker {
    backend: Arc<FakeBackend>,
    call_slots: Arc<tokio::sync::Semaphore>,
}

impl GrpcCallCredentialChecker for FixedBearerTokenOneCallSlotTestCredentialChecker {
    type Backend = FakeBackend;
    type OpenCallSlotPermit = tokio::sync::OwnedSemaphorePermit;

    async fn check_call_credentials_and_return_store_backend(
        &self,
        headers: &HeaderMap,
        extensions: &Extensions,
    ) -> Result<
        AuthenticatedGrpcCallWithOpenCallSlotGuard<FakeBackend, tokio::sync::OwnedSemaphorePermit>,
        Status,
    > {
        let call = FixedBearerTokenTestCredentialChecker(Arc::clone(&self.backend))
            .check_call_credentials_and_return_store_backend(headers, extensions)
            .await?;
        let open_call_slot_permit = Arc::clone(&self.call_slots)
            .try_acquire_owned()
            .map_err(|_| Status::resource_exhausted("call slot"))?;
        Ok(AuthenticatedGrpcCallWithOpenCallSlotGuard {
            project_nix_store_backend: call.project_nix_store_backend,
            open_call_slot_permit,
            caller_lost_access_future: Box::pin(futures::future::pending()),
        })
    }
}

struct FixedBearerTokenOneShotSenderTestCredentialChecker {
    backend: Arc<FakeBackend>,
    caller_lost_access_receiver: std::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

impl GrpcCallCredentialChecker for FixedBearerTokenOneShotSenderTestCredentialChecker {
    type Backend = FakeBackend;
    type OpenCallSlotPermit = ();

    async fn check_call_credentials_and_return_store_backend(
        &self,
        headers: &HeaderMap,
        extensions: &Extensions,
    ) -> Result<AuthenticatedGrpcCallWithOpenCallSlotGuard<FakeBackend, ()>, Status> {
        let call = FixedBearerTokenTestCredentialChecker(Arc::clone(&self.backend))
            .check_call_credentials_and_return_store_backend(headers, extensions)
            .await?;
        let caller_lost_access_receiver = self
            .caller_lost_access_receiver
            .lock()
            .expect("receiver lock")
            .take()
            .expect("one call per checker");
        Ok(AuthenticatedGrpcCallWithOpenCallSlotGuard {
            caller_lost_access_future: Box::pin(async move {
                let _ = caller_lost_access_receiver.await;
            }),
            ..call
        })
    }
}

async fn start_nix_store_server_with_caller_lost_access_sender_on_random_port_and_connect() -> (
    JoinSet<Result<(), tonic::transport::Error>>,
    Channel,
    tokio::sync::oneshot::Sender<()>,
) {
    let (caller_lost_access_sender, caller_lost_access_receiver) = tokio::sync::oneshot::channel();
    let (server, channel) =
        start_nix_store_server_with_credential_checker_on_random_port_and_connect(
            FixedBearerTokenOneShotSenderTestCredentialChecker {
                backend: Arc::new(FakeBackend(Journal::new())),
                caller_lost_access_receiver: std::sync::Mutex::new(Some(
                    caller_lost_access_receiver,
                )),
            },
        )
        .await;
    (server, channel, caller_lost_access_sender)
}

async fn start_nix_store_server_on_random_port_and_connect()
-> (JoinSet<Result<(), tonic::transport::Error>>, Channel) {
    start_nix_store_server_with_credential_checker_on_random_port_and_connect(
        FixedBearerTokenTestCredentialChecker(Arc::new(FakeBackend(Journal::new()))),
    )
    .await
}

async fn start_nix_store_server_with_credential_checker_on_random_port_and_connect<
    C: GrpcCallCredentialChecker,
>(
    checker: C,
) -> (JoinSet<Result<(), tonic::transport::Error>>, Channel) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("address");
    let service = AuthenticatedNixGrpcStoreService::new(Arc::new(checker));
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

struct EmptyRequestBodyThatNeverEnds;

impl http_body::Body for EmptyRequestBodyThatNeverEnds {
    type Data = Bytes;
    type Error = Status;

    fn poll_frame(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Status>>> {
        Poll::Pending
    }
}

#[tokio::test]
async fn unauthenticated_call_with_never_ending_body_is_refused_without_reading_body() {
    let service = AuthenticatedNixGrpcStoreService::new(Arc::new(
        FixedBearerTokenTestCredentialChecker(Arc::new(FakeBackend(Journal::new()))),
    ));
    let request = http::Request::builder()
        .method("POST")
        .uri("/nix.remote.NixRemote/StoreInfo")
        .header("content-type", "application/grpc")
        .body(EmptyRequestBodyThatNeverEnds)
        .expect("request");
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        service.nix_remote_grpc_service().call(request),
    )
    .await
    .expect("refused without waiting for the body")
    .expect("response");
    assert_eq!(
        response
            .headers()
            .get("grpc-status")
            .and_then(|value| value.to_str().ok()),
        Some("16")
    );
}

#[tokio::test]
async fn authenticated_query_valid_paths_over_message_limit_is_refused_with_out_of_range() {
    let (_server, channel) = start_nix_store_server_on_random_port_and_connect().await;
    let mut client = NixRemoteClient::new(channel);
    let path = fake_store_known_store_path().to_string();
    let paths = vec![path.clone(); 17 * 1024 * 1024 / path.len() + 1];
    let status = client
        .query_valid_paths(wrap_message_in_request_with_accepted_bearer_token(
            QueryValidPathsRequest {
                paths,
                substitute: false,
            },
        ))
        .await
        .expect_err("refused");
    assert_eq!(status.code(), Code::OutOfRange);
}

fn query_valid_paths_request_with_encoded_length(encoded_length: usize) -> QueryValidPathsRequest {
    let path = fake_store_known_store_path().to_string();
    let mut request = QueryValidPathsRequest {
        paths: vec![path.clone(); (encoded_length - 64) / (path.len() + 2)],
        substitute: false,
    };
    let remaining = encoded_length - prost::Message::encoded_len(&request);
    let filler_length = (remaining.saturating_sub(4)..remaining)
        .find(|length| {
            1 + prost::encoding::encoded_len_varint(*length as u64) + length == remaining
        })
        .expect("filler length");
    request
        .paths
        .push(path.repeat(filler_length / path.len() + 1)[..filler_length].to_owned());
    assert_eq!(prost::Message::encoded_len(&request), encoded_length);
    request
}

#[tokio::test]
async fn query_valid_paths_of_exactly_the_message_limit_is_decoded_and_one_byte_more_is_out_of_range()
 {
    let (_server, channel) = start_nix_store_server_on_random_port_and_connect().await;
    let mut client = NixRemoteClient::new(channel);
    let at_limit = client
        .query_valid_paths(wrap_message_in_request_with_accepted_bearer_token(
            query_valid_paths_request_with_encoded_length(MAX_DECODED_GRPC_MESSAGE_SIZE_BYTES),
        ))
        .await
        .map(|_| ())
        .map_err(|status| status.code());
    assert_eq!(at_limit, Ok(()));
    let over_limit = client
        .query_valid_paths(wrap_message_in_request_with_accepted_bearer_token(
            query_valid_paths_request_with_encoded_length(MAX_DECODED_GRPC_MESSAGE_SIZE_BYTES + 1),
        ))
        .await
        .expect_err("refused");
    assert_eq!(over_limit.code(), Code::OutOfRange);
}

#[tokio::test]
async fn streaming_response_body_holds_open_call_slot_until_dropped() {
    let (_server, channel) =
        start_nix_store_server_with_credential_checker_on_random_port_and_connect(
            FixedBearerTokenOneCallSlotTestCredentialChecker {
                backend: Arc::new(FakeBackend(Journal::new())),
                call_slots: Arc::new(tokio::sync::Semaphore::new(1)),
            },
        )
        .await;
    let mut scheduler = SchedulerClient::new(channel.clone());
    let mut client = NixRemoteClient::new(channel);
    let (request_sender, request_stream) = futures::channel::mpsc::unbounded::<ClientMsgs>();
    let open_stream = scheduler
        .schedule(wrap_message_in_request_with_accepted_bearer_token(
            request_stream,
        ))
        .await
        .expect("schedule")
        .into_inner();
    let status = client
        .store_info(wrap_message_in_request_with_accepted_bearer_token(
            StoreInfoRequest {},
        ))
        .await
        .expect_err("no free call slot");
    assert_eq!(status.code(), Code::ResourceExhausted);
    drop(request_sender);
    drop(open_stream);
    tokio::time::timeout(Duration::from_secs(5), async {
        while client
            .store_info(wrap_message_in_request_with_accepted_bearer_token(
                StoreInfoRequest {},
            ))
            .await
            .is_err()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("call slot released after the stream was dropped");
}

#[derive(Default)]
struct LargestWriteSizeRecordingWriter {
    largest_write: usize,
}

impl AsyncWrite for LargestWriteSizeRecordingWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.largest_write = self.largest_write.max(data.len());
        Poll::Ready(Ok(data.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn highly_compressed_piece_is_written_in_slices_no_larger_than_decoder_output_size() {
    let piece = ZstdStreamCompressor::new()
        .expect("compressor")
        .compress(&vec![0; 64 * 1024 * 1024])
        .expect("compress");
    assert!(piece.len() < 1024 * 1024);
    let mut writer = LargestWriteSizeRecordingWriter::default();
    decompress_zstd_stream_to_writer(futures::stream::iter([Ok(piece)]), &mut writer)
        .await
        .expect("decompress");
    assert!(writer.largest_write > 0);
    assert!(writer.largest_write <= DCtx::out_size());
}

#[tokio::test]
async fn zstd_stream_over_per_message_limit_decompresses_unchanged_to_slow_pipe_writer() {
    let data: Vec<u8> = (0..32 * 1024 * 1024u32)
        .map(|index| (index % 251) as u8)
        .collect();
    let mut compressor = ZstdStreamCompressor::new().expect("compressor");
    let pieces: Vec<Result<Vec<u8>, Status>> = data
        .chunks(256 * 1024)
        .map(|chunk| Ok(compressor.compress(chunk).expect("compress")))
        .collect();
    let (writer, mut reader) = tokio::io::duplex(64 * 1024);
    let (decompressed, received) = tokio::join!(
        decompress_zstd_stream_to_writer(futures::stream::iter(pieces), writer),
        async {
            let mut received = Vec::new();
            reader.read_to_end(&mut received).await.map(|_| received)
        }
    );
    decompressed.expect("decompress");
    assert!(received.expect("read") == data);
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
        decompressor
            .decompress_piece_to_async_writer(&frame.data, &mut nar)
            .await
            .expect("decompress");
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
        decompressor
            .decompress_piece_to_async_writer(&reply.data, &mut received)
            .await
            .expect("decompress");
    }
    assert_eq!(received[..8], SERVER_MAGIC.to_le_bytes());
}

#[tokio::test]
async fn connect_stream_ends_with_permission_denied_after_access_lost_future_completes() {
    let (_server, channel, caller_lost_access_sender) =
        start_nix_store_server_with_caller_lost_access_sender_on_random_port_and_connect().await;
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
        decompressor
            .decompress_piece_to_async_writer(&reply.data, &mut received)
            .await
            .expect("decompress");
    }
    drop(caller_lost_access_sender);
    let status = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Err(status) = replies.message().await {
                return status;
            }
        }
    })
    .await
    .expect("connect stream ends within 5 seconds after access is lost");
    assert_eq!(status.code(), Code::PermissionDenied);
}

#[tokio::test]
async fn add_multiple_to_store_upload_returns_permission_denied_when_access_lost_before_response_exists()
 {
    let (_server, channel, caller_lost_access_sender) =
        start_nix_store_server_with_caller_lost_access_sender_on_random_port_and_connect().await;
    let mut client = NixRemoteClient::new(channel);
    let mut compressor = ZstdStreamCompressor::new().expect("compressor");
    let first_chunk = AddMultipleChunk {
        repair: false,
        check_sigs: false,
        data: compressor
            .compress(&1u64.to_le_bytes()[..4])
            .expect("compress"),
    };
    let chunks = futures::stream::iter([first_chunk]).chain(futures::stream::pending());
    let mut upload = JoinSet::new();
    upload.spawn(async move {
        client
            .add_multiple_to_store(wrap_message_in_request_with_accepted_bearer_token(chunks))
            .await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(caller_lost_access_sender);
    let status = tokio::time::timeout(Duration::from_secs(5), upload.join_next())
        .await
        .expect("upload answers within 5 seconds after access is lost")
        .expect("upload task")
        .expect("upload task")
        .expect_err("permission denied");
    assert_eq!(status.code(), Code::PermissionDenied);
}

#[tokio::test]
async fn get_build_log_streams_zstd_chunks_that_decompress_to_build_log() {
    let (_server, channel) = start_nix_store_server_on_random_port_and_connect().await;
    let mut client = NixRemoteClient::new(channel);
    let mut chunks = client
        .get_build_log(wrap_message_in_request_with_accepted_bearer_token(
            GetBuildLogRequest {
                drv_path: fake_store_known_build_log_drv_store_path().to_string(),
            },
        ))
        .await
        .expect("build log")
        .into_inner();
    let mut decompressor = ZstdStreamPieceDecompressor::new().expect("decompressor");
    let mut log = Vec::new();
    let mut received_chunks = 0;
    while let Some(chunk) = chunks.message().await.expect("chunk") {
        received_chunks += 1;
        decompressor
            .decompress_piece_to_async_writer(&chunk.data, &mut log)
            .await
            .expect("decompress");
    }
    assert!(received_chunks > 1);
    assert_eq!(log, fake_store_known_derivation_build_log_bytes());
}

#[tokio::test]
async fn get_build_log_for_derivation_without_build_log_returns_not_found() {
    let (_server, channel) = start_nix_store_server_on_random_port_and_connect().await;
    let mut client = NixRemoteClient::new(channel);
    let status = client
        .get_build_log(wrap_message_in_request_with_accepted_bearer_token(
            GetBuildLogRequest {
                drv_path: "r0c60rjpjk8apx4c92k9453cj7wcaq0k-unknown.drv".into(),
            },
        ))
        .await
        .expect_err("not found");
    assert_eq!(status.code(), Code::NotFound);
}

#[tokio::test]
async fn query_missing_reports_derivation_targets_as_will_build_and_unknown_store_paths_as_unknown_by_base_name()
 {
    let (_server, channel) = start_nix_store_server_on_random_port_and_connect().await;
    let mut client = NixRemoteClient::new(channel);
    let drv = fake_store_known_build_log_drv_store_path().to_string();
    let reply = client
        .query_missing(wrap_message_in_request_with_accepted_bearer_token(
            QueryMissingRequest {
                targets: vec![
                    format!("/nix/store/{drv}^out"),
                    format!("/nix/store/{}", fake_store_known_store_path()),
                    "/nix/store/r0c60rjpjk8apx4c92k9453cj7wcaq0k-unknown".into(),
                ],
            },
        ))
        .await
        .expect("query missing")
        .into_inner();
    assert_eq!(reply.will_build, vec![drv]);
    assert!(reply.will_substitute.is_empty());
    assert_eq!(
        reply.unknown,
        vec!["r0c60rjpjk8apx4c92k9453cj7wcaq0k-unknown".to_owned()]
    );
}
