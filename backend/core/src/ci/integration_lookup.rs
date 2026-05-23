/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Resolve named integrations for organizations.
//!
//! The `integration` table stores per-org named records of forge integrations.
//! Outbound reporting is now driven by `ForgeStatusReport` actions that
//! reference an integration id directly (issue #262); the per-project link
//! table is gone.

use super::reporter::{CiReporter, NoopCiReporter};
use crate::types::*;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
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

/// Compatibility shim for entry-point CI reporting paths that have not yet
/// been migrated to `ForgeStatusReport` actions. Always returns
/// [`NoopCiReporter`] now that the `project_integration` link table is gone.
pub async fn resolve_outbound_reporter_for_project(
    _state: &Arc<ServerState>,
    _project_id: ProjectId,
) -> Arc<dyn CiReporter> {
    Arc::new(NoopCiReporter)
}

/// Stable name used for the auto-managed `forge_type=github` integration rows.
pub const GITHUB_APP_INTEGRATION_NAME: &str = "github";
/// Stable display name shown in dropdowns for the auto-managed GitHub App rows.
pub const GITHUB_APP_INTEGRATION_DISPLAY_NAME: &str = "GitHub";

/// Idempotently create the inbound + outbound `forge_type=github` integration
/// rows for `org_id`. Used by the App-install hook to materialise the rows
/// that triggers and ForgeStatusReport actions reference.
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
            .append_query_results([Vec::<entity::integration::Model>::new()])
            .append_query_results([vec![github_row(IntegrationKind::Inbound)]])
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
            .append_query_results([vec![github_row(IntegrationKind::Inbound)]])
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
