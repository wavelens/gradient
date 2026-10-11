/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::ErrorCode;
use crate::refusal::{Refusal, RefusalStatus};

#[derive(Debug, thiserror::Error)]
pub(crate) enum CacheRefusal {
    #[error("API key not allowed from this source IP")]
    ApiKeyWrongSourceIp,
    #[error("Authentication required to access this cache")]
    AuthenticationRequired,
    #[error("Signature not yet computed")]
    SignaturePending,
    #[error("NarHash not recorded")]
    NarHashNotStored,
    #[error("NarSize not recorded")]
    NarSizeNotStored,
    #[error("FileHash not recorded")]
    FileHashNotStored,
    #[error("FileSize not recorded")]
    FileSizeNotStored,
    #[error("Missing file hash")]
    MissingFileHash,
    #[error("Missing file size")]
    MissingFileSize,
    #[error("Cache is disabled")]
    CacheDisabled,
    #[error("No pending invitation for this user")]
    NoPendingInvitation,
    #[error("Anonymous cache access is disabled.")]
    AnonymousAccessDisabled,
    #[error("Server is at connection capacity; retry later.")]
    ConnectionCapacityReached,
    #[error("Too many connections from your IP; retry later.")]
    TooManyConnectionsFromIp,
    #[error("User not in Cache")]
    NotACacheMember,
    #[error("Cannot remove the last Admin from the cache.")]
    LastAdminRemoval,
    #[error("missing ?hash=")]
    MissingHashQuery,
    #[error("{0}")]
    InvalidHashInUrl(String),
    #[error("max_storage_gb must be 0 (unlimited) or at least 1")]
    NegativeMaxStorage,
    #[error("Invalid display name: {reason}")]
    InvalidDisplayName { reason: String },
    #[error("Built-in roles (Admin, Write, View) cannot be modified.")]
    BuiltinRoleNotModifiable,
    #[error("State-managed roles cannot be modified via the API.")]
    ManagedRoleNotModifiable,
    #[error("Built-in roles (Admin, Write, View) cannot be deleted.")]
    BuiltinRoleNotDeletable,
    #[error("State-managed roles cannot be deleted via the API.")]
    ManagedRoleNotDeletable,
    #[error("Role is still assigned to members. Reassign them before deleting the role.")]
    RoleAssignedToMembers,
    #[error("Role is still granted to a team. Change the team grant before deleting the role.")]
    RoleGrantedToTeam,
    #[error("Substituter URL is required.")]
    UpstreamUrlRequired,
    #[error("Substituter URL must start with http:// or https://.")]
    UpstreamUrlWithoutHttpScheme,
    #[error("Public key is required for an Http binary cache.")]
    HttpPublicKeyRequired,
    #[error(
        "An API key requires an https:// upstream URL so the key is not transmitted in cleartext."
    )]
    ApiKeyOverCleartext,
    #[error("Remote cache name is required for a Gradient Proto upstream.")]
    RemoteCacheNameRequired,
    #[error("Remote cache name is invalid.")]
    RemoteCacheNameInvalid,
    #[error("Remote cache name may only contain letters, digits, '-', '_', and '.'.")]
    RemoteCacheNameBadCharacters,
    #[error("A cache cannot be its own upstream")]
    CacheIsOwnUpstream,
    #[error("Only HTTP binary-cache upstreams can be tested.")]
    UpstreamNotHttp,
    #[error("{0}")]
    MultipartFieldUnreadable(String),
    #[error("invalid narinfo JSON: {reason}")]
    InvalidNarinfoJson { reason: String },
    #[error("missing narinfo part")]
    MissingNarinfoPart,
    #[error("missing nar part")]
    MissingNarPart,
    #[error("nar size mismatch: declared {declared} bytes, got {got}")]
    UploadedNarSizeMismatch { declared: i64, got: u64 },
    #[error("nar size mismatch: declared {declared} bytes, staged {staged}")]
    StagedNarSizeMismatch { declared: i64, staged: u64 },
    #[error("nar content verification failed: {reason}")]
    NarVerificationFailed { reason: String },
    #[error("nar exceeds max upload size of {max} bytes")]
    NarTooLarge { max: u64 },
    #[error("invalid store hash")]
    InvalidStoreHash,
}

impl Refusal for CacheRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::AuthenticationRequired => RefusalStatus::Unauthorized,
            Self::ApiKeyWrongSourceIp
            | Self::AnonymousAccessDisabled
            | Self::BuiltinRoleNotModifiable
            | Self::ManagedRoleNotModifiable
            | Self::BuiltinRoleNotDeletable
            | Self::ManagedRoleNotDeletable => RefusalStatus::Forbidden,
            Self::SignaturePending
            | Self::NarHashNotStored
            | Self::NarSizeNotStored
            | Self::FileHashNotStored
            | Self::FileSizeNotStored => RefusalStatus::NotFound,
            Self::LastAdminRemoval => RefusalStatus::Conflict,
            Self::NarTooLarge { .. } => RefusalStatus::PayloadTooLarge,
            Self::ConnectionCapacityReached | Self::TooManyConnectionsFromIp => {
                RefusalStatus::ServiceUnavailable
            }
            Self::MissingFileHash
            | Self::MissingFileSize
            | Self::CacheDisabled
            | Self::NoPendingInvitation
            | Self::NotACacheMember
            | Self::MissingHashQuery
            | Self::InvalidHashInUrl(_)
            | Self::NegativeMaxStorage
            | Self::InvalidDisplayName { .. }
            | Self::RoleAssignedToMembers
            | Self::RoleGrantedToTeam
            | Self::UpstreamUrlRequired
            | Self::UpstreamUrlWithoutHttpScheme
            | Self::HttpPublicKeyRequired
            | Self::ApiKeyOverCleartext
            | Self::RemoteCacheNameRequired
            | Self::RemoteCacheNameInvalid
            | Self::RemoteCacheNameBadCharacters
            | Self::CacheIsOwnUpstream
            | Self::UpstreamNotHttp
            | Self::MultipartFieldUnreadable(_)
            | Self::InvalidNarinfoJson { .. }
            | Self::MissingNarinfoPart
            | Self::MissingNarPart
            | Self::UploadedNarSizeMismatch { .. }
            | Self::StagedNarSizeMismatch { .. }
            | Self::NarVerificationFailed { .. }
            | Self::InvalidStoreHash => RefusalStatus::BadRequest,
        }
    }

    fn code(&self) -> ErrorCode {
        match self {
            Self::ApiKeyWrongSourceIp => ErrorCode::FORBIDDEN_SOURCE_IP,
            Self::MultipartFieldUnreadable(_)
            | Self::InvalidNarinfoJson { .. }
            | Self::MissingNarinfoPart
            | Self::MissingNarPart
            | Self::UploadedNarSizeMismatch { .. }
            | Self::StagedNarSizeMismatch { .. }
            | Self::NarVerificationFailed { .. }
            | Self::InvalidStoreHash => ErrorCode::INPUT_VALIDATION,
            other => other.status().default_code(),
        }
    }
}
