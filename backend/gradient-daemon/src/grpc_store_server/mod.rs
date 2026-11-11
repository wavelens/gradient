/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod nix_worker_protocol_payload_serialization;
mod zstd_stream_compression;

#[cfg(test)]
mod tests;

#[expect(
    clippy::allow_attributes_without_reason,
    reason = "tonic-prost-build generated this code."
)]
pub mod proto {
    tonic::include_proto!("nix.remote");
}

use crate::backend::Backend;
use crate::server::{next_conn, serve_stream};
use crate::store_features_beyond_nix_daemon_protocol::StoreFeaturesBeyondDaemonProtocol;
use futures::stream::BoxStream;
use futures::{Stream, StreamExt as _, TryStreamExt as _};
use harmonia_protocol::ProtocolVersion;
use harmonia_protocol::daemon::wire::parse_add_multiple_to_store;
use harmonia_protocol::daemon::wire::types2::{BuildMode, BuildResult, BuildResultInner};
use harmonia_protocol::daemon::{
    DaemonError, DaemonResult, DaemonStore, HandshakeDaemonStore, ResultLog, TrustLevel,
};
use harmonia_protocol::de::NixReader;
use harmonia_protocol::log::{Field, LogMessage, ResultType};
use harmonia_store_path::{StorePathHash, StorePathSet};
use nix_worker_protocol_payload_serialization::{
    parse_query_missing_target_string_to_derived_path,
    parse_store_path_and_basic_derivation_from_store_path_base_name_and_worker_protocol_bytes,
    parse_store_path_base_name_from_grpc_request,
    serialize_build_result_for_client_worker_protocol_version,
    serialize_unkeyed_valid_path_info_to_worker_protocol_1_16_bytes,
};
use proto::build_derivation_chunk::Event;
use proto::nix_remote_server::{NixRemote, NixRemoteServer};
use proto::scheduler_server::{Scheduler, SchedulerServer};
use proto::*;
use std::future::{Future, ready};
use std::pin::pin;
use std::sync::Arc;
use tokio::io::{AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::task::JoinSet;
use tonic::metadata::MetadataMap;
use tonic::{Extensions, Request, Response, Status, Streaming};
use zstd_stream_compression::{ZstdStreamCompressor, ZstdStreamPieceDecompressor};

const UNCOMPRESSED_NAR_BYTES_PER_FETCH_NARS_FRAME: usize = 256 * 1024;
const UNCOMPRESSED_BUILD_LOG_BYTES_PER_GET_BUILD_LOG_CHUNK: usize = 256 * 1024;

pub trait GrpcCallCredentialChecker: Send + Sync + 'static {
    type Backend: Backend<Handler: HandshakeDaemonStore<Store: StoreFeaturesBeyondDaemonProtocol>>;

    fn check_call_credentials_and_return_store_backend(
        &self,
        metadata: &MetadataMap,
        extensions: &Extensions,
    ) -> impl Future<Output = Result<Arc<Self::Backend>, Status>> + Send;
}

pub struct AuthenticatedNixGrpcStoreService<C> {
    callers: Arc<C>,
}

impl<C> Clone for AuthenticatedNixGrpcStoreService<C> {
    fn clone(&self) -> Self {
        Self {
            callers: Arc::clone(&self.callers),
        }
    }
}

fn nix_daemon_error_to_grpc_status(error: DaemonError) -> Status {
    Status::internal(error.to_string())
}

async fn finish_result_log_discarding_messages_and_turn_daemon_error_into_grpc_status<T>(
    log: impl ResultLog<Output = DaemonResult<T>>,
) -> Result<T, Status> {
    let mut log = pin!(log);
    while log.next().await.is_some() {}
    log.await.map_err(nix_daemon_error_to_grpc_status)
}

impl<C: GrpcCallCredentialChecker> AuthenticatedNixGrpcStoreService<C> {
    pub fn new(callers: Arc<C>) -> Self {
        Self { callers }
    }

    pub fn nix_remote_grpc_service(&self) -> NixRemoteServer<Self> {
        NixRemoteServer::new(self.clone())
            .max_decoding_message_size(usize::MAX)
            .max_encoding_message_size(usize::MAX)
    }

    pub fn scheduler_grpc_service(&self) -> SchedulerServer<Self> {
        SchedulerServer::new(self.clone())
    }

    async fn authenticate_grpc_call_and_return_nix_store(
        &self,
        metadata: &MetadataMap,
        extensions: &Extensions,
    ) -> Result<<<C::Backend as Backend>::Handler as HandshakeDaemonStore>::Store, Status> {
        let backend = self
            .callers
            .check_call_credentials_and_return_store_backend(metadata, extensions)
            .await?;
        finish_result_log_discarding_messages_and_turn_daemon_error_into_grpc_status(
            backend.handler(next_conn(None)).handshake(),
        )
        .await
    }
}

async fn decompress_zstd_stream_to_writer(
    pieces: impl Stream<Item = Result<Vec<u8>, Status>> + Send,
    mut writer: impl AsyncWrite + Unpin + Send,
) -> Result<(), Status> {
    let mut decompressor = ZstdStreamPieceDecompressor::new()?;
    let mut pieces = pin!(pieces);
    while let Some(piece) = pieces.try_next().await? {
        writer.write_all(&decompressor.decompress(&piece)?).await?;
    }
    writer.shutdown().await?;
    Ok(())
}

fn serve_connect_rpc_nix_daemon_stream<B: Backend>(
    backend: Arc<B>,
    chunks: Streaming<Chunk>,
) -> BoxStream<'static, Result<Chunk, Status>> {
    let (client_end, daemon_end) = tokio::io::duplex(1 << 20);
    let mut daemon_and_forwarding_tasks_stopped_on_response_stream_drop = JoinSet::new();
    daemon_and_forwarding_tasks_stopped_on_response_stream_drop.spawn(serve_stream(
        backend,
        next_conn(None),
        daemon_end,
    ));
    let (mut from_daemon, to_daemon) = tokio::io::split(client_end);
    daemon_and_forwarding_tasks_stopped_on_response_stream_drop.spawn(async move {
        let pieces = chunks.map_ok(|chunk| chunk.data);
        if let Err(error) = decompress_zstd_stream_to_writer(pieces, to_daemon).await {
            tracing::debug!(%error, "grpc connect stream ended");
        }
    });
    async_stream::try_stream! {
        let mut compressor = ZstdStreamCompressor::new()?;
        let mut buffer = vec![0; 64 * 1024];
        loop {
            let read = from_daemon.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            yield Chunk { data: compressor.compress(&buffer[..read])? };
        }
        drop(daemon_and_forwarding_tasks_stopped_on_response_stream_drop);
    }
    .boxed()
}

fn log_message_to_build_derivation_event(message: LogMessage) -> Option<Event> {
    match message {
        LogMessage::Message(message) => Some(Event::LogLine(message.text.to_vec())),
        LogMessage::Result(result) => {
            match (result.result_type, result.fields.into_iter().next()) {
                (ResultType::BuildLogLine, Some(Field::String(line))) => {
                    Some(Event::LogLine(line.to_vec()))
                }
                (ResultType::SetPhase, Some(Field::String(phase))) => {
                    Some(Event::Phase(String::from_utf8_lossy(&phase).into_owned()))
                }
                _ => None,
            }
        }
        LogMessage::StartActivity(_) | LogMessage::StopActivity(_) => None,
    }
}

async fn get_output_path_infos_for_finished_build_derivation<S: DaemonStore>(
    store: &mut S,
    result: &BuildResult,
) -> Result<Vec<PathInfo>, Status> {
    let BuildResultInner::Success(success) = &result.inner else {
        return Ok(Vec::new());
    };
    let mut infos = Vec::new();
    for realisation in success.built_outputs.values() {
        let path = &realisation.out_path;
        let info = finish_result_log_discarding_messages_and_turn_daemon_error_into_grpc_status(
            store.query_path_info(path),
        )
        .await?;
        if let Some(info) = info {
            infos.push(PathInfo {
                path: path.to_string(),
                info: serialize_unkeyed_valid_path_info_to_worker_protocol_1_16_bytes(&info)
                    .await?,
            });
        }
    }
    Ok(infos)
}

async fn answer_want_message<S: DaemonStore>(
    store: &mut S,
    want: Want,
    assign_id: u64,
) -> Result<SchedMsg, Status> {
    let mut cached = !want.out_keys.is_empty();
    for key in &want.out_keys {
        let hash = key
            .strip_suffix(".narinfo")
            .unwrap_or(key)
            .parse::<StorePathHash>()
            .map_err(|e| Status::invalid_argument(format!("{key}: {e}")))?;
        let path = finish_result_log_discarding_messages_and_turn_daemon_error_into_grpc_status(
            store.query_path_from_hash_part(&hash),
        )
        .await?;
        if path.is_none() {
            cached = false;
            break;
        }
    }
    let msg = if cached {
        sched_msg::Msg::Cached(Cached {
            drv_path: want.drv_path,
        })
    } else {
        sched_msg::Msg::Assigned(Assigned {
            drv_path: want.drv_path,
            worker_addr: String::new(),
            assign_id,
            worker_system: String::new(),
        })
    };
    Ok(SchedMsg { msg: Some(msg) })
}

#[tonic::async_trait]
impl<C: GrpcCallCredentialChecker> NixRemote for AuthenticatedNixGrpcStoreService<C> {
    type ConnectStream = BoxStream<'static, Result<Chunk, Status>>;
    type FetchNarsStream = BoxStream<'static, Result<NarFrame, Status>>;
    type BuildDerivationStream = BoxStream<'static, Result<BuildDerivationChunk, Status>>;
    type GetBuildLogStream = BoxStream<'static, Result<GetBuildLogChunk, Status>>;

    async fn connect(
        &self,
        request: Request<Streaming<Chunk>>,
    ) -> Result<Response<Self::ConnectStream>, Status> {
        let (metadata, extensions, chunks) = request.into_parts();
        let backend = self
            .callers
            .check_call_credentials_and_return_store_backend(&metadata, &extensions)
            .await?;
        Ok(Response::new(serve_connect_rpc_nix_daemon_stream(
            backend, chunks,
        )))
    }

    async fn query_valid_paths(
        &self,
        request: Request<QueryValidPathsRequest>,
    ) -> Result<Response<QueryValidPathsReply>, Status> {
        let (metadata, extensions, message) = request.into_parts();
        let mut store = self
            .authenticate_grpc_call_and_return_nix_store(&metadata, &extensions)
            .await?;
        let paths = message
            .paths
            .iter()
            .map(|path| parse_store_path_base_name_from_grpc_request(path))
            .collect::<Result<StorePathSet, _>>()?;
        let valid = finish_result_log_discarding_messages_and_turn_daemon_error_into_grpc_status(
            store.query_valid_paths(&paths, message.substitute),
        )
        .await?;
        Ok(Response::new(QueryValidPathsReply {
            paths: valid.iter().map(ToString::to_string).collect(),
        }))
    }

    async fn add_multiple_to_store(
        &self,
        request: Request<Streaming<AddMultipleChunk>>,
    ) -> Result<Response<AddMultipleReply>, Status> {
        let (metadata, extensions, mut chunks) = request.into_parts();
        let mut store = self
            .authenticate_grpc_call_and_return_nix_store(&metadata, &extensions)
            .await?;
        let Some(first) = chunks.message().await? else {
            return Ok(Response::new(AddMultipleReply {}));
        };
        let (repair, check_sigs) = (first.repair, first.check_sigs);
        let pieces =
            futures::stream::once(ready(Ok(first.data))).chain(chunks.map_ok(|chunk| chunk.data));
        let (upload, mut import) = tokio::io::duplex(1 << 20);
        let add = async {
            let mut reader = NixReader::builder()
                .set_version(ProtocolVersion::from_parts(1, 16))
                .build_buffered(&mut import);
            let items = parse_add_multiple_to_store(&mut reader)
                .await
                .map_err(nix_daemon_error_to_grpc_status)?;
            finish_result_log_discarding_messages_and_turn_daemon_error_into_grpc_status(
                store.add_multiple_to_store(repair, !check_sigs, items),
            )
            .await
        };
        tokio::try_join!(decompress_zstd_stream_to_writer(pieces, upload), add)?;
        Ok(Response::new(AddMultipleReply {}))
    }

    async fn query_path_infos(
        &self,
        request: Request<QueryPathInfosRequest>,
    ) -> Result<Response<QueryPathInfosReply>, Status> {
        let (metadata, extensions, message) = request.into_parts();
        let mut store = self
            .authenticate_grpc_call_and_return_nix_store(&metadata, &extensions)
            .await?;
        let mut infos = Vec::new();
        for base_name in message.paths {
            let path = parse_store_path_base_name_from_grpc_request(&base_name)?;
            let info =
                finish_result_log_discarding_messages_and_turn_daemon_error_into_grpc_status(
                    store.query_path_info(&path),
                )
                .await?;
            if let Some(info) = info {
                infos.push(PathInfo {
                    path: base_name,
                    info: serialize_unkeyed_valid_path_info_to_worker_protocol_1_16_bytes(&info)
                        .await?,
                });
            }
        }
        Ok(Response::new(QueryPathInfosReply { infos }))
    }

    async fn fetch_nars(
        &self,
        request: Request<FetchNarsRequest>,
    ) -> Result<Response<Self::FetchNarsStream>, Status> {
        let (metadata, extensions, message) = request.into_parts();
        let mut store = self
            .authenticate_grpc_call_and_return_nix_store(&metadata, &extensions)
            .await?;
        let paths = message
            .paths
            .iter()
            .map(|path| parse_store_path_base_name_from_grpc_request(path))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Response::new(
            async_stream::try_stream! {
                let mut compressor = ZstdStreamCompressor::new()?;
                let mut buffer = vec![0; UNCOMPRESSED_NAR_BYTES_PER_FETCH_NARS_FRAME];
                for (path_index, path) in (0u32..).zip(&paths) {
                    let nar = finish_result_log_discarding_messages_and_turn_daemon_error_into_grpc_status(
                        store.nar_from_path(path),
                    )
                    .await?;
                    let mut nar = pin!(nar);
                    loop {
                        let read = nar.read(&mut buffer).await?;
                        if read == 0 {
                            break;
                        }
                        yield NarFrame {
                            path_index,
                            data: compressor.compress(&buffer[..read])?,
                            eof: false,
                        };
                    }
                    yield NarFrame { path_index, data: Vec::new(), eof: true };
                }
            }
            .boxed(),
        ))
    }

    async fn build_derivation(
        &self,
        request: Request<BuildDerivationRequest>,
    ) -> Result<Response<Self::BuildDerivationStream>, Status> {
        let (metadata, extensions, message) = request.into_parts();
        let (drv_path, drv) =
            parse_store_path_and_basic_derivation_from_store_path_base_name_and_worker_protocol_bytes(
                &message.drv_path,
                &message.drv,
            )
            .await?;
        let mode = u16::try_from(message.build_mode)
            .ok()
            .and_then(|mode| BuildMode::try_from(mode).ok())
            .ok_or_else(|| {
                Status::invalid_argument(format!("build mode {}", message.build_mode))
            })?;
        let mut store = self
            .authenticate_grpc_call_and_return_nix_store(&metadata, &extensions)
            .await?;
        Ok(Response::new(
            async_stream::try_stream! {
                let result = {
                    let mut log = pin!(store.build_derivation(&drv_path, &drv, mode));
                    while let Some(message) = log.next().await {
                        if let Some(event) = log_message_to_build_derivation_event(message) {
                            yield BuildDerivationChunk { event: Some(event) };
                        }
                    }
                    log.await.map_err(nix_daemon_error_to_grpc_status)?
                };
                let outputs = get_output_path_infos_for_finished_build_derivation(&mut store, &result).await?;
                let result = serialize_build_result_for_client_worker_protocol_version(message.protocol, &result).await?;
                yield BuildDerivationChunk {
                    event: Some(Event::Done(BuildDerivationDone { result, outputs })),
                };
            }
            .boxed(),
        ))
    }

    async fn query_missing(
        &self,
        request: Request<QueryMissingRequest>,
    ) -> Result<Response<QueryMissingReply>, Status> {
        let (metadata, extensions, message) = request.into_parts();
        let mut store = self
            .authenticate_grpc_call_and_return_nix_store(&metadata, &extensions)
            .await?;
        let targets = message
            .targets
            .iter()
            .map(|target| parse_query_missing_target_string_to_derived_path(target))
            .collect::<Result<Vec<_>, _>>()?;
        let missing = finish_result_log_discarding_messages_and_turn_daemon_error_into_grpc_status(
            store.query_missing(&targets),
        )
        .await?;
        Ok(Response::new(QueryMissingReply {
            will_build: missing.will_build.iter().map(ToString::to_string).collect(),
            will_substitute: missing
                .will_substitute
                .iter()
                .map(ToString::to_string)
                .collect(),
            unknown: missing.unknown.iter().map(ToString::to_string).collect(),
            download_size: missing.download_size,
            nar_size: missing.nar_size,
        }))
    }

    async fn store_info(
        &self,
        request: Request<StoreInfoRequest>,
    ) -> Result<Response<StoreInfoReply>, Status> {
        let (metadata, extensions, _) = request.into_parts();
        let store = self
            .authenticate_grpc_call_and_return_nix_store(&metadata, &extensions)
            .await?;
        Ok(Response::new(StoreInfoReply {
            trusted: store
                .trust_level()
                .map(|level| matches!(level, TrustLevel::Trusted)),
        }))
    }

    async fn get_build_log(
        &self,
        request: Request<GetBuildLogRequest>,
    ) -> Result<Response<Self::GetBuildLogStream>, Status> {
        let (metadata, extensions, message) = request.into_parts();
        let mut store = self
            .authenticate_grpc_call_and_return_nix_store(&metadata, &extensions)
            .await?;
        let drv_path = parse_store_path_base_name_from_grpc_request(&message.drv_path)?;
        let log = store
            .read_build_log(&drv_path)
            .await
            .map_err(nix_daemon_error_to_grpc_status)?
            .ok_or_else(|| Status::not_found(format!("No build log for {drv_path}")))?;
        Ok(Response::new(
            async_stream::try_stream! {
                let mut compressor = ZstdStreamCompressor::new()?;
                for piece in log.chunks(UNCOMPRESSED_BUILD_LOG_BYTES_PER_GET_BUILD_LOG_CHUNK) {
                    yield GetBuildLogChunk { data: compressor.compress(piece)? };
                }
            }
            .boxed(),
        ))
    }
}

#[tonic::async_trait]
impl<C: GrpcCallCredentialChecker> Scheduler for AuthenticatedNixGrpcStoreService<C> {
    type ScheduleStream = BoxStream<'static, Result<SchedMsgs, Status>>;
    type WorkerSessionStream = BoxStream<'static, Result<SchedCmds, Status>>;

    async fn schedule(
        &self,
        request: Request<Streaming<ClientMsgs>>,
    ) -> Result<Response<Self::ScheduleStream>, Status> {
        let (metadata, extensions, mut batches) = request.into_parts();
        let mut store = self
            .authenticate_grpc_call_and_return_nix_store(&metadata, &extensions)
            .await?;
        Ok(Response::new(
            async_stream::try_stream! {
                let mut assign_ids = 1u64..;
                while let Some(batch) = batches.message().await? {
                    let mut answers = Vec::new();
                    for message in batch.msgs {
                        if let Some(client_msg::Msg::Want(want)) = message.msg {
                            let assign_id = assign_ids.next().unwrap_or_default();
                            answers.push(answer_want_message(&mut store, want, assign_id).await?);
                        }
                    }
                    if !answers.is_empty() {
                        yield SchedMsgs { msgs: answers };
                    }
                }
            }
            .boxed(),
        ))
    }

    async fn worker_session(
        &self,
        _request: Request<Streaming<WorkerMsgs>>,
    ) -> Result<Response<Self::WorkerSessionStream>, Status> {
        Err(Status::unimplemented("WorkerSession"))
    }
}
