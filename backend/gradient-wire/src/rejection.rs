/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::messages::{ClientMessage, ServerMessage};

/// Codes and texts are protocol. Keep `docs/src/contributors/proto/connection.md` in step with
/// every new variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Rejection {
    #[error("expected InitConnection")]
    ExpectedInitConnection,
    #[error("expected AuthResponse")]
    ExpectedAuthResponse,
    #[error("expected Authenticate")]
    ExpectedAuthenticate,
    #[error("no valid peer tokens provided")]
    NoValidPeerTokens,
    #[error("unknown worker id or wrong token")]
    UnknownWorkerOrWrongToken,
    #[error("server is not accepting connections")]
    NotAcceptingConnections,
    #[error("worker is deactivated")]
    WorkerDeactivated,
    #[error("unknown worker")]
    UnknownWorker,
    #[error("no project grants this team's workers")]
    NoProjectGrant,
    #[error("no project authorizes this worker")]
    NoProjectAuthorization,
    #[error("Push not allowed on a read-only cache session")]
    PushOnCacheSession,
    #[error("only CacheQuery and NarRequest are allowed on a cache session")]
    NotAllowedOnCacheSession,
    #[error("no connection token stored; register the worker again")]
    NoStoredToken,
    #[error("project has no cache subscribed")]
    NoCacheSubscribed,
    #[error("worker already connected")]
    AlreadyConnected,
    #[error("scheduler unavailable")]
    SchedulerUnavailable,
}

impl Rejection {
    pub const fn code(self) -> u16 {
        match self {
            Self::ExpectedInitConnection
            | Self::ExpectedAuthResponse
            | Self::ExpectedAuthenticate => 400,
            Self::NoValidPeerTokens | Self::UnknownWorkerOrWrongToken => 401,
            Self::NotAcceptingConnections
            | Self::WorkerDeactivated
            | Self::UnknownWorker
            | Self::NoProjectGrant
            | Self::NoProjectAuthorization
            | Self::PushOnCacheSession
            | Self::NotAllowedOnCacheSession
            | Self::NoStoredToken => 403,
            Self::NoCacheSubscribed => 495,
            Self::AlreadyConnected => 496,
            Self::SchedulerUnavailable => 503,
        }
    }
}

impl From<Rejection> for ServerMessage {
    fn from(rejection: Rejection) -> Self {
        Self::Reject {
            code: rejection.code(),
            reason: rejection.to_string(),
        }
    }
}

impl From<Rejection> for ClientMessage {
    fn from(rejection: Rejection) -> Self {
        Self::Reject {
            code: rejection.code(),
            reason: rejection.to_string(),
        }
    }
}
