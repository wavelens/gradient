/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use harmonia_protocol::ProtocolVersion;
use harmonia_protocol::daemon::wire::types2::BuildResult;
use harmonia_protocol::de::{NixRead as _, NixReader};
use harmonia_protocol::ser::{NixSerialize, NixWrite as _, NixWriter};
use harmonia_protocol::valid_path_info::UnkeyedValidPathInfo;
use harmonia_store_derivation::derivation::BasicDerivation;
use harmonia_store_derivation::derived_path::DerivedPath;
use harmonia_store_path::{StoreDir, StorePath};
use std::io::Cursor;
use tokio::io::AsyncWriteExt as _;
use tonic::Status;

pub fn unpack_protocol_version(packed: u32) -> ProtocolVersion {
    let [_, _, major, minor] = packed.to_be_bytes();
    ProtocolVersion::from_parts(major, minor)
}

pub fn parse_store_path_base_name_from_grpc_request(base_name: &str) -> Result<StorePath, Status> {
    StorePath::from_base_path(base_name)
        .map_err(|e| Status::invalid_argument(format!("{base_name}: {e}")))
}

pub fn parse_query_missing_target_string_to_derived_path(
    target: &str,
) -> Result<DerivedPath, Status> {
    StoreDir::default()
        .parse::<DerivedPath>(target)
        .map_err(|e| Status::invalid_argument(e.to_string()))
}

async fn serialize_value_to_nix_worker_protocol_bytes_at_version<T>(
    version: ProtocolVersion,
    value: &T,
) -> Result<Vec<u8>, Status>
where
    T: NixSerialize + Send + Sync,
{
    let mut bytes = Vec::new();
    let mut writer = NixWriter::builder().set_version(version).build(&mut bytes);
    writer
        .write_value(value)
        .await
        .map_err(|e| Status::internal(e.to_string()))?;
    writer
        .flush()
        .await
        .map_err(|e| Status::internal(e.to_string()))?;
    drop(writer);
    Ok(bytes)
}

pub async fn serialize_unkeyed_valid_path_info_to_worker_protocol_1_16_bytes(
    info: &UnkeyedValidPathInfo,
) -> Result<Vec<u8>, Status> {
    serialize_value_to_nix_worker_protocol_bytes_at_version(
        ProtocolVersion::from_parts(1, 16),
        info,
    )
    .await
}

pub async fn serialize_build_result_for_client_worker_protocol_version(
    packed_version: u32,
    result: &BuildResult,
) -> Result<Vec<u8>, Status> {
    serialize_value_to_nix_worker_protocol_bytes_at_version(
        unpack_protocol_version(packed_version),
        result,
    )
    .await
}

pub async fn parse_store_path_and_basic_derivation_from_store_path_base_name_and_worker_protocol_bytes(
    base_name: &str,
    derivation: &[u8],
) -> Result<(StorePath, BasicDerivation), Status> {
    let path = parse_store_path_base_name_from_grpc_request(base_name)?;
    let mut bytes =
        serialize_value_to_nix_worker_protocol_bytes_at_version(ProtocolVersion::default(), &path)
            .await?;
    bytes.extend_from_slice(derivation);
    NixReader::builder()
        .build_buffered(Cursor::new(bytes))
        .read_value()
        .await
        .map_err(|e| Status::invalid_argument(format!("{base_name}: {e}")))
}
