/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::refusal::{Refusal, RefusalStatus};

#[derive(Debug, thiserror::Error)]
pub(crate) enum ProjectRefusal {
    #[error("Settings in the server configuration define this subscription. Make changes there.")]
    DeclaredInServerStateConfiguration,
    #[error("Project not subscribed to Cache")]
    NotSubscribedToCache,
    #[error("team workers are managed on the team page")]
    TeamWorkerManagedOnTeamPage,
    #[error("A project can't drain a worker. Drain it in the team or admin area.")]
    ProjectWorkerPageCannotDrainWorkerOnSharedMachine,
    #[error("{reason}")]
    InvalidSecretName { reason: String },
    #[error("secret value is empty")]
    EmptySecretValue,
    #[error("secret value is larger than 65536 bytes")]
    SecretValueTooLarge,
    #[error("secret \"{name}\" is managed by the declarative state")]
    ManagedSecret { name: String },
    #[error("project key unusable for sops: {reason}")]
    UnusableSopsKey { reason: String },
    #[error("Invalid worker URL: {reason}")]
    InvalidWorkerUrl { reason: String },
}

impl Refusal for ProjectRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::NotSubscribedToCache
            | Self::InvalidSecretName { .. }
            | Self::EmptySecretValue
            | Self::SecretValueTooLarge
            | Self::InvalidWorkerUrl { .. } => RefusalStatus::BadRequest,
            Self::TeamWorkerManagedOnTeamPage
            | Self::ProjectWorkerPageCannotDrainWorkerOnSharedMachine
            | Self::DeclaredInServerStateConfiguration
            | Self::ManagedSecret { .. }
            | Self::UnusableSopsKey { .. } => RefusalStatus::Conflict,
        }
    }
}
