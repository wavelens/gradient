/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::git_host_connections::{self, GitHostConnectionOwnerByDatabaseId};
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ActiveValue, ConnectionTrait, DbErr, IntoActiveModel, TransactionSession,
    TransactionTrait,
};

pub const GITHUB_APP_CONNECTION_DISPLAY_NAME: &str = "GitHub";

fn github_connection_name(account_login: Option<&str>, installation_id: i64) -> String {
    match account_login {
        Some(login) if !login.trim().is_empty() => {
            format!("github-{}", login.trim().to_ascii_lowercase())
        }
        _ => format!("github-{installation_id}"),
    }
}

async fn free_instance_name<C: ConnectionTrait>(
    db: &C,
    account_login: Option<&str>,
    installation_id: i64,
) -> Result<String, DbErr> {
    let preferred = github_connection_name(account_login, installation_id);
    let taken =
        git_host_connections::by_name(db, GitHostConnectionOwnerByDatabaseId::Instance, &preferred)
            .await?
            .is_some();
    Ok(if taken {
        github_connection_name(None, installation_id)
    } else {
        preferred
    })
}

pub async fn upsert_github_app_installation_git_host_connection<C: TransactionTrait>(
    db: &C,
    installation_id: i64,
    account_login: Option<&str>,
    creator: UserId,
) -> Result<MGitHostConnection, DbErr> {
    let txn = db.begin().await?;
    let connection =
        match git_host_connections::by_github_installation_id(&txn, installation_id).await? {
            Some(existing) => match account_login {
                Some(login) if existing.account_login.as_deref() != Some(login) => {
                    let mut active = existing.into_active_model();
                    active.account_login = ActiveValue::Set(Some(login.to_owned()));
                    active.update(&txn).await?
                }
                _ => existing,
            },
            None => {
                MGitHostConnection {
                    id: GitHostConnectionId::now_v7(),
                    project: None,
                    owner_team: None,
                    name: free_instance_name(&txn, account_login, installation_id).await?,
                    display_name: GITHUB_APP_CONNECTION_DISPLAY_NAME.to_owned(),
                    git_host_type: GitHostType::GitHub,
                    receives_events: true,
                    reports_status: true,
                    github_installation_id: Some(installation_id),
                    account_login: account_login.map(str::to_owned),
                    created_by: creator,
                    created_at: chrono::Utc::now().naive_utc(),
                    ..Default::default()
                }
                .into_active_model()
                .insert(&txn)
                .await?
            }
        };
    txn.commit().await?;
    Ok(connection)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::mock_db;

    fn instance_row(name: &str, installation_id: Option<i64>) -> MGitHostConnection {
        MGitHostConnection {
            id: GitHostConnectionId::now_v7(),
            name: name.into(),
            display_name: GITHUB_APP_CONNECTION_DISPLAY_NAME.into(),
            git_host_type: GitHostType::GitHub,
            receives_events: true,
            reports_status: true,
            github_installation_id: installation_id,
            account_login: Some("Acme-Corp".into()),
            ..Default::default()
        }
    }

    #[test]
    fn the_name_follows_the_account_login_or_the_installation_id() {
        assert_eq!(
            github_connection_name(Some("Acme-Corp"), 42),
            "github-acme-corp"
        );
        assert_eq!(github_connection_name(Some("  "), 42), "github-42");
        assert_eq!(github_connection_name(None, 42), "github-42");
    }

    #[tokio::test]
    async fn a_known_installation_keeps_its_connection() {
        let existing = instance_row("github-acme-corp", Some(42));
        let db = mock_db()
            .append_query_results([vec![existing.clone()]])
            .into_connection();

        let connection = upsert_github_app_installation_git_host_connection(
            &db,
            42,
            Some("Acme-Corp"),
            UserId::now_v7(),
        )
        .await
        .unwrap();
        assert_eq!(connection.id, existing.id);
    }

    #[tokio::test]
    async fn a_taken_name_falls_back_to_the_installation_id() {
        let db = mock_db()
            .append_query_results([Vec::<MGitHostConnection>::new()])
            .append_query_results([vec![instance_row("github-acme-corp", None)]])
            .append_query_results([vec![instance_row("github-42", Some(42))]])
            .into_connection();

        let connection = upsert_github_app_installation_git_host_connection(
            &db,
            42,
            Some("Acme-Corp"),
            UserId::now_v7(),
        )
        .await
        .unwrap();
        assert_eq!(connection.name, "github-42");
    }
}
