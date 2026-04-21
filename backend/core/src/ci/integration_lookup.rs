/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Resolve named integrations for organizations and projects.
//!
//! The `integration` table stores per-org named records of forge integrations.
//! Each project can reference a single inbound and a single outbound
//! integration via the `project_integration` link table.

use super::reporter::{CiReporter, GiteaReporter, NoopCiReporter};
use super::webhook::decrypt_webhook_secret;
use crate::types::*;
use sea_orm::EntityTrait;
use std::sync::Arc;
use tracing::warn;
use uuid::Uuid;

/// Numeric encoding of `integration.kind`.
#[repr(i16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationKind {
    Inbound = 0,
    Outbound = 1,
}

impl IntegrationKind {
    pub fn as_i16(self) -> i16 {
        self as i16
    }
}

/// Numeric encoding of `integration.forge_type`.
#[repr(i16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgeType {
    Gitea = 0,
    Forgejo = 1,
    GitLab = 2,
    GitHub = 3,
}

impl ForgeType {
    pub fn as_i16(self) -> i16 {
        self as i16
    }

    pub fn from_i16(v: i16) -> Option<Self> {
        match v {
            0 => Some(Self::Gitea),
            1 => Some(Self::Forgejo),
            2 => Some(Self::GitLab),
            3 => Some(Self::GitHub),
            _ => None,
        }
    }

    pub fn from_path_segment(s: &str) -> Option<Self> {
        match s {
            "gitea" => Some(Self::Gitea),
            "forgejo" => Some(Self::Forgejo),
            "gitlab" => Some(Self::GitLab),
            "github" => Some(Self::GitHub),
            _ => None,
        }
    }
}

/// Build a CI reporter for a project's configured **outbound** integration.
///
/// Returns [`NoopCiReporter`] when:
/// - the project has no `project_integration` row,
/// - the row has no `outbound_integration`,
/// - the integration is unreachable or its token cannot be decrypted,
/// - the forge type does not support outbound reporting yet (GitLab, GitHub App).
pub async fn resolve_outbound_reporter_for_project(
    state: &Arc<ServerState>,
    project_id: Uuid,
) -> Arc<dyn CiReporter> {
    use sea_orm::ColumnTrait;
    use sea_orm::QueryFilter;

    let link = match EProjectIntegration::find_by_id(project_id)
        .one(&state.db)
        .await
    {
        Ok(Some(l)) => l,
        Ok(None) => return Arc::new(NoopCiReporter),
        Err(e) => {
            warn!(error = %e, %project_id, "DB error looking up project_integration");
            return Arc::new(NoopCiReporter);
        }
    };

    let Some(outbound_id) = link.outbound_integration else {
        return Arc::new(NoopCiReporter);
    };

    let integration = match EIntegration::find_by_id(outbound_id)
        .filter(CIntegration::Kind.eq(IntegrationKind::Outbound.as_i16()))
        .one(&state.db)
        .await
    {
        Ok(Some(i)) => i,
        Ok(None) => return Arc::new(NoopCiReporter),
        Err(e) => {
            warn!(error = %e, %outbound_id, "DB error looking up outbound integration");
            return Arc::new(NoopCiReporter);
        }
    };

    let forge = match ForgeType::from_i16(integration.forge_type) {
        Some(f) => f,
        None => return Arc::new(NoopCiReporter),
    };

    // Decrypt access token if present.
    let token = match integration.access_token.as_deref() {
        Some(enc) => match decrypt_webhook_secret(&state.cli.crypt_secret_file, enc) {
            Ok(t) => Some(t),
            Err(e) => {
                warn!(error = %e, integration_id = %integration.id, "Failed to decrypt integration access token");
                None
            }
        },
        None => None,
    };

    match forge {
        ForgeType::Gitea | ForgeType::Forgejo => {
            let Some(base_url) = integration.endpoint_url.as_deref().filter(|s| !s.is_empty())
            else {
                warn!(integration_id = %integration.id, "Gitea/Forgejo outbound integration missing endpoint_url");
                return Arc::new(NoopCiReporter);
            };
            let Some(token) = token else {
                return Arc::new(NoopCiReporter);
            };
            match GiteaReporter::new(base_url.to_string(), token.expose().to_string()) {
                Ok(r) => Arc::new(r),
                Err(e) => {
                    warn!(error = %e, "Failed to build GiteaReporter");
                    Arc::new(NoopCiReporter)
                }
            }
        }
        ForgeType::GitHub => {
            // Outbound GitHub uses the server-configured GitHub App. The
            // per-integration record only records *presence* of GitHub support
            // for the org; there's no per-integration token.
            // TODO: wire up GitHub App installation-token-based reporter here.
            Arc::new(NoopCiReporter)
        }
        ForgeType::GitLab => {
            // TODO: implement GitLabReporter.
            Arc::new(NoopCiReporter)
        }
    }
}
