/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::{ErrorCode, WebResult};
use crate::permissions::CachePermission;
use crate::refusal::{Refusal, RefusalStatus};
use gradient_types::MCache;
use gradient_util::upstream_cache_credential::UpstreamCacheCredentialError;

pub(crate) fn ensure_cache_enabled(cache: MCache) -> WebResult<MCache> {
    if cache.active {
        Ok(cache)
    } else {
        Err(CacheRefusal::CacheDisabled.into())
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CacheRefusal {
    #[error("Authentication required to access this cache")]
    AuthenticationRequired,
    #[error("Downloads from this cache need the \"{}\" permission.", CachePermission::ReadStore.permission_label())]
    ReadCachePermissionRequired,
    #[error("Signature not yet computed")]
    SignaturePending,
    #[error("{0} not recorded")]
    NotRecorded(&'static str),
    #[error("Cache is disabled")]
    CacheDisabled,
    #[error("Anonymous cache access is disabled.")]
    AnonymousAccessDisabled,
    #[error("Server is at connection capacity; retry later.")]
    ConnectionCapacityReached,
    #[error("Too many connections from your IP; retry later.")]
    TooManyConnectionsFromIp,
    #[error("missing ?hash=")]
    MissingHashQuery,
    #[error("max_storage_gb must be 0 (unlimited) or between 1 and 2147483647")]
    MaxStorageOutOfRange,
    #[error("Substituter URL is required.")]
    UpstreamUrlRequired,
    #[error("Invalid upstream cache URL: {0}")]
    UpstreamCacheUrlInvalid(String),
    #[error("Use only files under `nar/` for upstream NAR paths.")]
    InvalidNarFilePath,
    #[error("Public key is required for an Http binary cache.")]
    HttpPublicKeyRequired,
    #[error(
        "An API key requires an https:// upstream URL so the key is not transmitted in cleartext."
    )]
    ApiKeyOverCleartext,
    #[error("Place the user name and password in `credential`, not in the upstream URL.")]
    UpstreamCacheUrlContainsCredentials,
    #[error("An upstream URL can't contain a query. Send the token in `credential`.")]
    CacheUrlContainsQueryString,
    #[error(
        "Credentials need an https:// upstream URL. URLs without https:// send them in plaintext."
    )]
    UpstreamCacheCredentialSentOverPlainHttp,
    #[error("{0}")]
    InvalidUpstreamCacheCredential(#[from] UpstreamCacheCredentialError),
    #[error(
        "Send the credentials again, or remove them with `{{\"scheme\": \"none\"}}`, if the upstream URL moves to another scheme, host, or port."
    )]
    StoredCredentialWouldBeSentToNewUpstreamCacheHost,
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
    #[error("missing {0} part")]
    MissingPart(&'static str),
    #[error("nar size mismatch: declared {declared} bytes, got {got}")]
    UploadedNarSizeMismatch { declared: i64, got: u64 },
    #[error("nar size mismatch: declared {declared} bytes, staged {staged}")]
    StagedNarSizeMismatch { declared: i64, staged: u64 },
    #[error("nar content verification failed: {reason}")]
    NarVerificationFailed { reason: String },
    #[error("nar exceeds max upload size of {max} bytes")]
    NarTooLarge { max: u64 },
    #[error("The approved mode can't grant more access rights than the project requested.")]
    GrantedModeHasMoreAccessThanRequested,
    #[error("invalid store hash")]
    InvalidStoreHash,
    #[error("The store hash in the URL doesn't match the store path in the narinfo.")]
    UrlPathStoreHashNarInfoStorePathHashMismatch,
    #[error("Keep `file_size` and `nar_size` non-negative.")]
    NegativeNarInfoSize,
    #[error(
        "This account reached the maximum number of caches. Ask an administrator to raise the limit."
    )]
    CacheLimitReached,
    #[error(
        "This cache reached the maximum number of upstream caches ({}). Delete an upstream cache before adding another.",
        gradient_db::caches::upstream::MAX_UPSTREAM_CACHE_COUNT
    )]
    MaximumUpstreamCacheCountReached,
    #[error(
        "Cache {cache} is over the storage limit. Remove stored paths before an upload, or raise the storage limit."
    )]
    CacheStorageLimitReachedForUpload { cache: String },
}

pub(crate) async fn refuse_upload_if_no_storage_space_left(
    db: &impl sea_orm::ConnectionTrait,
    cache: &MCache,
    instance_limit_gb: u32,
) -> WebResult<()> {
    if gradient_db::caches::capacity::storage_limits_leave_less_than_required_free_space_for_upload(
        db,
        cache,
        instance_limit_gb,
    )
    .await?
    {
        return Err(CacheRefusal::CacheStorageLimitReachedForUpload {
            cache: cache.name.clone(),
        }
        .into());
    }
    Ok(())
}

impl Refusal for CacheRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::AuthenticationRequired | Self::ReadCachePermissionRequired => {
                RefusalStatus::Unauthorized
            }
            Self::AnonymousAccessDisabled
            | Self::CacheLimitReached
            | Self::MaximumUpstreamCacheCountReached => RefusalStatus::Forbidden,
            Self::SignaturePending | Self::NotRecorded(_) => RefusalStatus::NotFound,
            Self::NarTooLarge { .. } => RefusalStatus::PayloadTooLarge,
            Self::CacheStorageLimitReachedForUpload { .. } => RefusalStatus::Conflict,
            Self::ConnectionCapacityReached | Self::TooManyConnectionsFromIp => {
                RefusalStatus::ServiceUnavailable
            }
            Self::CacheDisabled
            | Self::MissingHashQuery
            | Self::MaxStorageOutOfRange
            | Self::UpstreamUrlRequired
            | Self::UpstreamCacheUrlInvalid(_)
            | Self::InvalidNarFilePath
            | Self::HttpPublicKeyRequired
            | Self::ApiKeyOverCleartext
            | Self::UpstreamCacheUrlContainsCredentials
            | Self::CacheUrlContainsQueryString
            | Self::UpstreamCacheCredentialSentOverPlainHttp
            | Self::InvalidUpstreamCacheCredential(_)
            | Self::StoredCredentialWouldBeSentToNewUpstreamCacheHost
            | Self::RemoteCacheNameRequired
            | Self::RemoteCacheNameInvalid
            | Self::RemoteCacheNameBadCharacters
            | Self::CacheIsOwnUpstream
            | Self::UpstreamNotHttp
            | Self::MultipartFieldUnreadable(_)
            | Self::InvalidNarinfoJson { .. }
            | Self::MissingPart(_)
            | Self::UploadedNarSizeMismatch { .. }
            | Self::StagedNarSizeMismatch { .. }
            | Self::NarVerificationFailed { .. }
            | Self::GrantedModeHasMoreAccessThanRequested
            | Self::InvalidStoreHash
            | Self::UrlPathStoreHashNarInfoStorePathHashMismatch
            | Self::NegativeNarInfoSize => RefusalStatus::BadRequest,
        }
    }

    fn code(&self) -> ErrorCode {
        match self {
            Self::MultipartFieldUnreadable(_)
            | Self::InvalidNarinfoJson { .. }
            | Self::MissingPart(_)
            | Self::UploadedNarSizeMismatch { .. }
            | Self::StagedNarSizeMismatch { .. }
            | Self::NarVerificationFailed { .. }
            | Self::GrantedModeHasMoreAccessThanRequested
            | Self::InvalidStoreHash
            | Self::UrlPathStoreHashNarInfoStorePathHashMismatch
            | Self::NegativeNarInfoSize => ErrorCode::INPUT_VALIDATION,
            Self::CacheLimitReached => ErrorCode::CACHE_LIMIT_REACHED,
            other => other.status().default_code(),
        }
    }

    fn missing_permission_wire_names(&self) -> Vec<&'static str> {
        match self {
            Self::ReadCachePermissionRequired => vec![CachePermission::ReadStore.as_wire_name()],
            _ => Vec::new(),
        }
    }
}
