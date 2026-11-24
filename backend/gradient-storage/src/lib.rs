/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod admission;
pub mod context;
pub mod debug_info;
pub mod digest;
pub mod eval_cache_segment;
pub mod hot;
mod layout;
pub mod log;
pub mod log_chunk;
mod multipart;
pub mod nar;
pub mod nar_extract;
pub mod partial;
pub mod passthrough;
mod s3_http_transport_with_stall_failure;
pub mod sgr;
pub mod source_nar;
mod timed;

pub use self::context::StorageCtx;
pub use self::debug_info::{BuildIdEntry, scan_build_ids};
pub use self::digest::{
    ClientClaimedNarUploadHashesAndSizes, ServerComputedNarAndCompressedFileHashesAndSizes,
    UploadedStorePathAndContentAddress, VerifyError, file_hash_sri,
    verify_zstd_nar_stream_sha256_and_sizes_against_claims,
};
#[cfg(feature = "testing")]
pub use self::digest::{
    compress_nar_bytes_and_verify_stream,
    compress_nar_bytes_as_two_zstd_frames_and_verify_hashes_and_sizes,
};
pub use self::hot::{HotNarCache, HotNarStats};
pub use self::log::*;
pub use self::nar::*;
pub use self::partial::{PartialStore, PartialWriter, StagedFile};
