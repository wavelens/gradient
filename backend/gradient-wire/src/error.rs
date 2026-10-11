/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::session::handshake::Intent;
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq)]
pub enum HandshakeError {
    #[error("connection closed before InitConnection")]
    ClosedBeforeInitConnection,
    #[error("send InitAck")]
    InitAckNotSent,
    #[error("server rejected connection (code {code}): {reason}")]
    ServerRejected { code: u16, reason: String },
    #[error("unexpected intent during init: {intent:?}")]
    UnexpectedIntentDuringInit { intent: Intent },
}
