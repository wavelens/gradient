/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::refusal::{Refusal, RefusalStatus};

#[derive(Debug, thiserror::Error)]
pub(crate) enum BuildRequestRefusal {
    #[error("Manifest must contain at least one file")]
    ManifestEmpty,
    #[error("Duplicate path in manifest: {path}")]
    DuplicateManifestPath { path: String },
    #[error("Negative size for {path}")]
    NegativeFileSize { path: String },
    #[error("Manifest total size overflows i64")]
    ManifestSizeOverflow,
    #[error("Manifest total size exceeds limit of {limit} bytes")]
    ManifestTooLarge { limit: usize },
    #[error("Upload session already dispatched")]
    UploadSessionAlreadyDispatched,
    #[error("Upload session expired")]
    UploadSessionExpired,
    #[error("{count} blobs still missing")]
    BlobsStillMissing { count: usize },
    #[error("Specify at most one of `ref` and `rev`")]
    RefAndRevTogether,
    #[error("`rev` must be a 40-character hex hash")]
    InvalidRev,
    #[error("Invalid repository URL: {reason}")]
    InvalidRepositoryUrl { reason: String },
    #[error("Invalid multipart payload: {reason}")]
    InvalidMultipartPayload { reason: String },
    #[error("Missing field name")]
    MissingFieldName,
    #[error("hash not in session.missing: {name}")]
    BlobNotExpected { name: String },
    #[error("Failed to read field bytes: {reason}")]
    UnreadableFieldBytes { reason: String },
    #[error("hash mismatch for {name}")]
    BlobHashMismatch { name: String },
    #[error("Failed to read nar: {reason}")]
    UnreadableNar { reason: String },
    #[error("missing `nar` field")]
    MissingNarField,
    #[error("empty `nar` field")]
    EmptyNarField,
    #[error("invalid upload id")]
    InvalidUploadId,
    #[error("source upload exceeds max size of {max} bytes")]
    SourceUploadTooLarge { max: u64 },
    #[error("no staged source upload: {reason}")]
    NoStagedSourceUpload { reason: String },
    #[error("empty source upload")]
    EmptySourceUpload,
}

impl Refusal for BuildRequestRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::UploadSessionAlreadyDispatched | Self::BlobsStillMissing { .. } => {
                RefusalStatus::Conflict
            }
            Self::UploadSessionExpired => RefusalStatus::Gone,
            Self::ManifestSizeOverflow
            | Self::ManifestTooLarge { .. }
            | Self::SourceUploadTooLarge { .. } => RefusalStatus::PayloadTooLarge,
            Self::ManifestEmpty
            | Self::DuplicateManifestPath { .. }
            | Self::NegativeFileSize { .. }
            | Self::RefAndRevTogether
            | Self::InvalidRev
            | Self::InvalidRepositoryUrl { .. }
            | Self::InvalidMultipartPayload { .. }
            | Self::MissingFieldName
            | Self::BlobNotExpected { .. }
            | Self::UnreadableFieldBytes { .. }
            | Self::BlobHashMismatch { .. }
            | Self::UnreadableNar { .. }
            | Self::MissingNarField
            | Self::EmptyNarField
            | Self::InvalidUploadId
            | Self::NoStagedSourceUpload { .. }
            | Self::EmptySourceUpload => RefusalStatus::BadRequest,
        }
    }
}
