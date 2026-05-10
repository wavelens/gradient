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

use super::reporter::{
    CiReporter, GiteaReporter, GithubAppReporter, GitlabReporter, NoopCiReporter,
};
use super::webhook::decrypt_webhook_secret;
use crate::types::*;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use std::fs;
use std::sync::Arc;
use tracing::warn;

/// Numeric encoding of `integration.kind`.
#[repr(i16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoPrimitive, TryFromPrimitive)]
pub enum IntegrationKind {
    Inbound = 0,
    Outbound = 1,
}

/// Numeric encoding of `integration.forge_type`.
#[repr(i16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoPrimitive, TryFromPrimitive)]
pub enum ForgeType {
    Gitea = 0,
    Forgejo = 1,
    GitLab = 2,
    GitHub = 3,
}

impl ForgeType {
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
/// - the integration is missing required fields (endpoint URL or access token).
pub async fn resolve_outbound_reporter_for_project(
    state: &Arc<ServerState>,
    project_id: ProjectId,
) -> Arc<dyn CiReporter> {
    let link = match EProjectIntegration::find_by_id(project_id)
        .one(&state.worker_db)
        .await
    {
        Ok(Some(l)) => Some(l),
        Ok(None) => None,
        Err(e) => {
            warn!(error = %e, %project_id, "DB error looking up project_integration");
            None
        }
    };

    let Some(outbound_id) = link.as_ref().and_then(|l| l.outbound_integration) else {
        return Arc::new(NoopCiReporter);
    };

    let integration = match EIntegration::find_by_id(outbound_id)
        .filter(CIntegration::Kind.eq(i16::from(IntegrationKind::Outbound)))
        .one(&state.worker_db)
        .await
    {
        Ok(Some(i)) => i,
        Ok(None) => return Arc::new(NoopCiReporter),
        Err(e) => {
            warn!(error = %e, %outbound_id, "DB error looking up outbound integration");
            return Arc::new(NoopCiReporter);
        }
    };

    let forge = match ForgeType::try_from(integration.forge_type) {
        Ok(f) => f,
        Err(_) => return Arc::new(NoopCiReporter),
    };

    // Decrypt access token if present.
    let token = match integration.access_token.as_deref() {
        Some(enc) => match decrypt_webhook_secret(&state.config.secrets.crypt_secret_file, enc) {
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
            let Some(base_url) = integration
                .endpoint_url
                .as_deref()
                .filter(|s| !s.is_empty())
            else {
                warn!(integration_id = %integration.id, "Gitea/Forgejo outbound integration missing endpoint_url");
                return Arc::new(NoopCiReporter);
            };
            let Some(token) = token else {
                return Arc::new(NoopCiReporter);
            };
            match GiteaReporter::new(
                state.http.clone(),
                base_url.to_string(),
                token.expose().to_string(),
            ) {
                Ok(r) => Arc::new(r),
                Err(e) => {
                    warn!(error = %e, "Failed to build GiteaReporter");
                    Arc::new(NoopCiReporter)
                }
            }
        }
        ForgeType::GitHub => build_github_app_reporter_for_project(state, project_id)
            .await
            .unwrap_or_else(|| Arc::new(NoopCiReporter)),
        ForgeType::GitLab => {
            let Some(base_url) = integration
                .endpoint_url
                .as_deref()
                .filter(|s| !s.is_empty())
            else {
                warn!(integration_id = %integration.id, "GitLab outbound integration missing endpoint_url");
                return Arc::new(NoopCiReporter);
            };
            let Some(token) = token else {
                warn!(integration_id = %integration.id, "GitLab outbound integration missing access token");
                return Arc::new(NoopCiReporter);
            };
            match GitlabReporter::new(
                state.http.clone(),
                base_url.to_string(),
                token.expose().to_string(),
            ) {
                Ok(r) => Arc::new(r),
                Err(e) => {
                    warn!(error = %e, "Failed to build GitlabReporter");
                    Arc::new(NoopCiReporter)
                }
            }
        }
    }
}

/// Builds a [`GithubAppReporter`] for a project when the server has the
/// GitHub App fully configured and the project's organization has a stored
/// `github_installation_id`.
///
/// Returns `None` when either precondition isn't satisfied so the caller can
/// fall back to a noop reporter. Opt-in is conveyed by the project pointing
/// `outbound_integration` at the GitHub App row — repo URL is not consulted.
async fn build_github_app_reporter_for_project(
    state: &Arc<ServerState>,
    project_id: ProjectId,
) -> Option<Arc<dyn CiReporter>> {
    let github_app = state.config.github_app.clone()?;

    let project = EProject::find_by_id(project_id)
        .one(&state.worker_db)
        .await
        .ok()
        .flatten()?;

    let org = EOrganization::find_by_id(project.organization)
        .one(&state.worker_db)
        .await
        .ok()
        .flatten()?;

    let installation_id = org.github_installation_id?;

    let pem = match fs::read_to_string(&github_app.private_key_file) {
        Ok(s) => s,
        Err(e) => {
            warn!(
                error = %e,
                path = %github_app.private_key_file,
                "Failed to read GitHub App private key for outbound CI reporting"
            );
            return None;
        }
    };

    // GitHub Enterprise support deferred — no production user yet. When
    // adding it, derive `api_base_url` from the installation account host or
    // from a server-config field instead of hardcoding the empty default.
    match GithubAppReporter::new(
        state.http.clone(),
        "",
        github_app.app_id,
        pem,
        installation_id,
    ) {
        Ok(r) => Some(Arc::new(r)),
        Err(e) => {
            warn!(error = %e, "Failed to build GithubAppReporter");
            None
        }
    }
}

/// Stable name used for the auto-managed `forge_type=github` integration rows.
pub const GITHUB_APP_INTEGRATION_NAME: &str = "github";
/// Stable display name shown in dropdowns for the auto-managed GitHub App rows.
pub const GITHUB_APP_INTEGRATION_DISPLAY_NAME: &str = "GitHub";

/// Idempotently create the inbound + outbound `forge_type=github` integration
/// rows for `org_id`. Used by the App-install hook to materialise the rows
/// that triggers and project_integration links reference.
///
/// Rows carry no per-row credentials — the App's private key and the org's
/// `github_installation_id` are the credentials at runtime. `created_by` is
/// set to `creator` (typically the org's `created_by`) to satisfy the FK.
pub async fn ensure_github_app_integrations<C: ConnectionTrait>(
    db: &C,
    org_id: OrganizationId,
    creator: UserId,
) -> Result<(), sea_orm::DbErr> {
    for kind in [IntegrationKind::Inbound, IntegrationKind::Outbound] {
        let existing_github = EIntegration::find()
            .filter(CIntegration::Organization.eq(org_id))
            .filter(CIntegration::Kind.eq(i16::from(kind)))
            .filter(CIntegration::ForgeType.eq(i16::from(ForgeType::GitHub)))
            .one(db)
            .await?;
        if existing_github.is_some() {
            continue;
        }
        let name_clash = EIntegration::find()
            .filter(CIntegration::Organization.eq(org_id))
            .filter(CIntegration::Kind.eq(i16::from(kind)))
            .filter(CIntegration::Name.eq(GITHUB_APP_INTEGRATION_NAME))
            .one(db)
            .await?;
        if name_clash.is_some() {
            warn!(
                %org_id,
                kind = ?kind,
                "Cannot seed GitHub App integration row: another integration already \
                 uses the reserved name '{}'. Rename it to enable GitHub App support.",
                GITHUB_APP_INTEGRATION_NAME
            );
            continue;
        }
        AIntegration {
            id: Set(IntegrationId::now_v7()),
            organization: Set(org_id),
            name: Set(GITHUB_APP_INTEGRATION_NAME.into()),
            display_name: Set(GITHUB_APP_INTEGRATION_DISPLAY_NAME.into()),
            kind: Set(i16::from(kind)),
            forge_type: Set(i16::from(ForgeType::GitHub)),
            secret: Set(None),
            endpoint_url: Set(None),
            access_token: Set(None),
            created_by: Set(creator),
            created_at: Set(chrono::Utc::now().naive_utc()),
        }
        .insert(db)
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod ensure_tests {
    use super::*;
    use chrono::NaiveDateTime;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use uuid::Uuid;

    fn org() -> OrganizationId {
        OrganizationId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap())
    }
    fn user() -> UserId {
        UserId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap())
    }

    fn github_row(kind: IntegrationKind) -> entity::integration::Model {
        entity::integration::Model {
            id: IntegrationId::now_v7(),
            organization: org(),
            name: GITHUB_APP_INTEGRATION_NAME.into(),
            display_name: GITHUB_APP_INTEGRATION_DISPLAY_NAME.into(),
            kind: i16::from(kind),
            forge_type: i16::from(ForgeType::GitHub),
            secret: None,
            endpoint_url: None,
            access_token: None,
            created_by: user(),
            created_at: NaiveDateTime::default(),
        }
    }

    #[tokio::test]
    async fn creates_both_rows_when_none_exist() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // inbound: SELECT (none) → INSERT
            .append_query_results([Vec::<entity::integration::Model>::new()])
            .append_query_results([vec![github_row(IntegrationKind::Inbound)]])
            // outbound: SELECT (none) → INSERT
            .append_query_results([Vec::<entity::integration::Model>::new()])
            .append_query_results([vec![github_row(IntegrationKind::Outbound)]])
            .into_connection();

        ensure_github_app_integrations(&db, org(), user())
            .await
            .expect("ensure should succeed");
    }

    #[tokio::test]
    async fn skips_kinds_that_already_exist() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // inbound: SELECT (exists) → skip insert
            .append_query_results([vec![github_row(IntegrationKind::Inbound)]])
            // outbound: SELECT (exists) → skip insert
            .append_query_results([vec![github_row(IntegrationKind::Outbound)]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        ensure_github_app_integrations(&db, org(), user())
            .await
            .expect("ensure should be idempotent");
    }
}
