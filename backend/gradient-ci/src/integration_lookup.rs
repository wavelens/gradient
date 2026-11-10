/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::github_installation;
use gradient_entity::ids::GithubInstallationId;
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel, QueryFilter,
    TransactionSession, TransactionTrait,
    sea_query::{Expr, OnConflict},
};
use tracing::warn;

pub use gradient_entity::integration::IntegrationKind;

pub const GITHUB_APP_INTEGRATION_DISPLAY_NAME: &str = "GitHub";

fn github_integration_name(account_login: Option<&str>, installation_id: i64) -> String {
    match account_login {
        Some(login) if !login.trim().is_empty() => {
            format!("github-{}", login.trim().to_ascii_lowercase())
        }
        _ => format!("github-{installation_id}"),
    }
}

pub async fn upsert_github_installation<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    installation_id: i64,
    account_login: Option<&str>,
    creator: UserId,
) -> Result<GithubInstallationId, sea_orm::DbErr> {
    let id = GithubInstallationId::now_v7();
    let model = github_installation::ActiveModel {
        id: sea_orm::ActiveValue::Set(id),
        project: sea_orm::ActiveValue::Set(project),
        installation_id: sea_orm::ActiveValue::Set(installation_id),
        account_login: sea_orm::ActiveValue::Set(account_login.map(|s| s.to_string())),
        created_by: sea_orm::ActiveValue::Set(creator),
        created_at: sea_orm::ActiveValue::Set(chrono::Utc::now().naive_utc()),
    };

    let res = github_installation::Entity::insert(model)
        .on_conflict(
            OnConflict::columns([
                github_installation::Column::Project,
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

#[derive(Debug)]
pub struct InstallationLinkResult {
    pub installation: GithubInstallationId,
    pub integration_name: String,
    pub created_any_integration: bool,
}

pub async fn link_project_github_installation<C: TransactionTrait>(
    db: &C,
    project: ProjectId,
    installation_id: i64,
    account_login: Option<&str>,
    creator: UserId,
) -> Result<InstallationLinkResult, sea_orm::DbErr> {
    let txn = db.begin().await?;
    let installation =
        upsert_github_installation(&txn, project, installation_id, account_login, creator).await?;
    let integration_name = github_integration_name(account_login, installation_id);
    let created_any_integration =
        ensure_github_app_integrations(&txn, project, installation, &integration_name, creator)
            .await?;
    txn.commit().await?;

    Ok(InstallationLinkResult {
        installation,
        integration_name,
        created_any_integration,
    })
}

async fn ensure_github_app_integrations<C: ConnectionTrait>(
    db: &C,
    project_id: ProjectId,
    installation: GithubInstallationId,
    name: &str,
    creator: UserId,
) -> Result<bool, sea_orm::DbErr> {
    let mut created = false;
    for kind in [IntegrationKind::Inbound, IntegrationKind::Outbound] {
        let existing = EIntegration::find()
            .filter(CIntegration::Project.eq(project_id))
            .filter(CIntegration::Kind.eq(kind))
            .filter(CIntegration::GitHostType.eq(GitHostType::GitHub))
            .filter(CIntegration::GithubInstallation.eq(installation))
            .one(db)
            .await?;
        if existing.is_some() {
            continue;
        }

        let name_clash = EIntegration::find()
            .filter(CIntegration::Project.eq(project_id))
            .filter(CIntegration::Kind.eq(kind))
            .filter(CIntegration::Name.eq(name))
            .one(db)
            .await?;
        if name_clash.is_some() {
            warn!(%project_id, kind = ?kind, name, "GitHub App integration name already taken; skipping");
            continue;
        }

        MIntegration {
            id: IntegrationId::now_v7(),
            project: project_id,
            name: name.to_string(),
            display_name: GITHUB_APP_INTEGRATION_DISPLAY_NAME.to_string(),
            kind,
            git_host_type: GitHostType::GitHub,
            github_installation: Some(installation),
            created_by: creator,
            created_at: chrono::Utc::now().naive_utc(),
            ..Default::default()
        }
        .into_active_model()
        .insert(db)
        .await?;
        created = true;
    }

    Ok(created)
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
    use gradient_fixtures::mock::mock_db;
    use uuid::Uuid;

    fn project() -> ProjectId {
        ProjectId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap())
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
            project: project(),
            name: "github-acme-corp".into(),
            display_name: GITHUB_APP_INTEGRATION_DISPLAY_NAME.into(),
            kind,
            git_host_type: GitHostType::GitHub,
            github_installation: Some(installation()),
            created_by: user(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn creates_both_rows_when_none_exist() {
        let db = mock_db()
            .append_query_results([Vec::<gradient_entity::integration::Model>::new()])
            .append_query_results([Vec::<gradient_entity::integration::Model>::new()])
            .append_query_results([vec![github_row(IntegrationKind::Inbound)]])
            .append_query_results([Vec::<gradient_entity::integration::Model>::new()])
            .append_query_results([Vec::<gradient_entity::integration::Model>::new()])
            .append_query_results([vec![github_row(IntegrationKind::Outbound)]])
            .into_connection();

        let created = ensure_github_app_integrations(
            &db,
            project(),
            installation(),
            "github-acme-corp",
            user(),
        )
        .await
        .expect("ensure should succeed");
        assert!(created);
    }

    #[tokio::test]
    async fn skips_kinds_that_already_exist() {
        let db = mock_db()
            .append_query_results([vec![github_row(IntegrationKind::Inbound)]])
            .append_query_results([vec![github_row(IntegrationKind::Outbound)]])
            .into_connection();

        let created = ensure_github_app_integrations(
            &db,
            project(),
            installation(),
            "github-acme-corp",
            user(),
        )
        .await
        .expect("ensure should be idempotent");
        assert!(!created);
    }

    #[tokio::test]
    async fn linking_an_installation_stores_it_and_creates_its_integrations() {
        let stored = github_installation::Model {
            id: installation(),
            project: project(),
            installation_id: 42,
            account_login: Some("Acme-Corp".into()),
            created_by: user(),
            created_at: chrono::Utc::now().naive_utc(),
        };
        let db = mock_db()
            .append_query_results([vec![stored]])
            .append_query_results([Vec::<gradient_entity::integration::Model>::new()])
            .append_query_results([Vec::<gradient_entity::integration::Model>::new()])
            .append_query_results([vec![github_row(IntegrationKind::Inbound)]])
            .append_query_results([vec![github_row(IntegrationKind::Outbound)]])
            .into_connection();

        let link = link_project_github_installation(&db, project(), 42, Some("Acme-Corp"), user())
            .await
            .expect("linking should succeed");

        assert_eq!(link.installation, installation());
        assert_eq!(link.integration_name, "github-acme-corp");
        assert!(link.created_any_integration);
    }
}
