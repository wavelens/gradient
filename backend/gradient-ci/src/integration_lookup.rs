/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Resolve named forge integrations for organizations.

use gradient_entity::github_installation;
use gradient_entity::ids::GithubInstallationId;
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel, QueryFilter,
    sea_query::{Expr, OnConflict},
};
use tracing::warn;

pub use gradient_entity::integration::IntegrationKind;

/// Stable display name shown in dropdowns for the auto-managed GitHub App rows.
pub const GITHUB_APP_INTEGRATION_DISPLAY_NAME: &str = "GitHub";

/// Stable, per-installation integration name. Lowercased account login keeps it
/// URL/index-safe; falls back to the numeric installation id.
pub fn github_integration_name(account_login: Option<&str>, installation_id: i64) -> String {
    match account_login {
        Some(login) if !login.trim().is_empty() => {
            format!("github-{}", login.trim().to_ascii_lowercase())
        }
        _ => format!("github-{installation_id}"),
    }
}

/// Find-or-create the `github_installation` row for (org, installation_id),
/// refreshing `account_login` when newly known. Returns its id.
pub async fn upsert_github_installation<C: ConnectionTrait>(
    db: &C,
    org: OrganizationId,
    installation_id: i64,
    account_login: Option<&str>,
    creator: UserId,
) -> Result<GithubInstallationId, sea_orm::DbErr> {
    let id = GithubInstallationId::now_v7();
    let model = github_installation::ActiveModel {
        id: sea_orm::ActiveValue::Set(id),
        organization: sea_orm::ActiveValue::Set(org),
        installation_id: sea_orm::ActiveValue::Set(installation_id),
        account_login: sea_orm::ActiveValue::Set(account_login.map(|s| s.to_string())),
        created_by: sea_orm::ActiveValue::Set(creator),
        created_at: sea_orm::ActiveValue::Set(chrono::Utc::now().naive_utc()),
    };

    let res = github_installation::Entity::insert(model)
        .on_conflict(
            OnConflict::columns([
                github_installation::Column::Organization,
                github_installation::Column::InstallationId,
            ])
            .value(
                github_installation::Column::AccountLogin,
                Expr::cust("COALESCE(EXCLUDED.account_login, github_installation.account_login)"),
            )
            .to_owned(),
        )
        .exec_with_returning(db)
        .await?;

    Ok(res.id)
}

pub async fn ensure_github_app_integrations<C: ConnectionTrait>(
    db: &C,
    org_id: OrganizationId,
    installation: GithubInstallationId,
    name: &str,
    display_name: &str,
    creator: UserId,
) -> Result<(), sea_orm::DbErr> {
    for kind in [IntegrationKind::Inbound, IntegrationKind::Outbound] {
        let existing = EIntegration::find()
            .filter(CIntegration::Organization.eq(org_id))
            .filter(CIntegration::Kind.eq(kind))
            .filter(CIntegration::ForgeType.eq(ForgeType::GitHub))
            .filter(CIntegration::GithubInstallation.eq(installation))
            .one(db)
            .await?;
        if existing.is_some() {
            continue;
        }

        let name_clash = EIntegration::find()
            .filter(CIntegration::Organization.eq(org_id))
            .filter(CIntegration::Kind.eq(kind))
            .filter(CIntegration::Name.eq(name))
            .one(db)
            .await?;
        if name_clash.is_some() {
            warn!(%org_id, kind = ?kind, name, "GitHub App integration name already taken; skipping");
            continue;
        }

        MIntegration {
            id: IntegrationId::now_v7(),
            organization: org_id,
            name: name.to_string(),
            display_name: display_name.to_string(),
            kind,
            forge_type: ForgeType::GitHub,
            github_installation: Some(installation),
            created_by: creator,
            created_at: chrono::Utc::now().naive_utc(),
            ..Default::default()
        }
        .into_active_model()
        .insert(db)
        .await?;
    }

    Ok(())
}

#[cfg(test)]
mod name_tests {
    use super::github_integration_name;

    #[test]
    fn uses_account_login_when_present() {
        assert_eq!(
            github_integration_name(Some("Acme-Corp"), 42),
            "github-acme-corp"
        );
    }

    #[test]
    fn falls_back_to_installation_id() {
        assert_eq!(github_integration_name(None, 42), "github-42");
    }
}

#[cfg(test)]
mod ensure_tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use uuid::Uuid;

    fn org() -> OrganizationId {
        OrganizationId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap())
    }
    fn user() -> UserId {
        UserId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap())
    }
    fn installation() -> GithubInstallationId {
        GithubInstallationId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap())
    }

    fn github_row(kind: IntegrationKind) -> gradient_entity::integration::Model {
        gradient_entity::integration::Model {
            id: IntegrationId::now_v7(),
            organization: org(),
            name: "github-acme-corp".into(),
            display_name: GITHUB_APP_INTEGRATION_DISPLAY_NAME.into(),
            kind,
            forge_type: ForgeType::GitHub,
            github_installation: Some(installation()),
            created_by: user(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn creates_both_rows_when_none_exist() {
        // Per kind: (1) installation filter → empty, (2) name clash → empty, (3) insert result
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Inbound: installation filter → none
            .append_query_results([Vec::<gradient_entity::integration::Model>::new()])
            // Inbound: name clash → none
            .append_query_results([Vec::<gradient_entity::integration::Model>::new()])
            // Inbound: insert result
            .append_query_results([vec![github_row(IntegrationKind::Inbound)]])
            // Outbound: installation filter → none
            .append_query_results([Vec::<gradient_entity::integration::Model>::new()])
            // Outbound: name clash → none
            .append_query_results([Vec::<gradient_entity::integration::Model>::new()])
            // Outbound: insert result
            .append_query_results([vec![github_row(IntegrationKind::Outbound)]])
            .into_connection();

        ensure_github_app_integrations(
            &db,
            org(),
            installation(),
            "github-acme-corp",
            GITHUB_APP_INTEGRATION_DISPLAY_NAME,
            user(),
        )
        .await
        .expect("ensure should succeed");
    }

    #[tokio::test]
    async fn skips_kinds_that_already_exist() {
        // Per kind: (1) installation filter → found → skip
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Inbound: installation filter → found
            .append_query_results([vec![github_row(IntegrationKind::Inbound)]])
            // Outbound: installation filter → found
            .append_query_results([vec![github_row(IntegrationKind::Outbound)]])
            .into_connection();

        ensure_github_app_integrations(
            &db,
            org(),
            installation(),
            "github-acme-corp",
            GITHUB_APP_INTEGRATION_DISPLAY_NAME,
            user(),
        )
        .await
        .expect("ensure should be idempotent");
    }
}
