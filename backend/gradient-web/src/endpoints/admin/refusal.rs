/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::refusal::{Refusal, RefusalStatus};

#[derive(Debug, thiserror::Error)]
pub(crate) enum AdminRefusal {
    #[error("host must not be empty")]
    EmptyHost,
    #[error("host must contain only alphanumerics, '.' and '-'")]
    InvalidHostCharacters,
    #[error("manifest state invalid or expired")]
    InvalidManifestState,
    #[error("Pending credentials")]
    NoPendingCredentials,
    #[error("unknown format '{format}': expected 'nix' or 'json'")]
    UnknownFormat { format: String },
}

impl Refusal for AdminRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::EmptyHost
            | Self::InvalidHostCharacters
            | Self::InvalidManifestState
            | Self::UnknownFormat { .. } => RefusalStatus::BadRequest,
            Self::NoPendingCredentials => RefusalStatus::NotFound,
        }
    }
}
