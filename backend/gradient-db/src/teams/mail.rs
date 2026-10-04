/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};

pub async fn resolve_mail_recipients<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    recipients: &[String],
) -> Result<Vec<String>, DbErr> {
    let mut resolved = Vec::new();
    for recipient in recipients {
        match recipient.strip_prefix("team:") {
            Some(name) => resolved.extend(team_emails(db, project, name).await?),
            None => resolved.push(recipient.clone()),
        }
    }
    resolved.sort();
    resolved.dedup();
    Ok(resolved)
}

pub async fn team_reaches_project_users<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    name: &str,
) -> Result<bool, DbErr> {
    Ok(granted_team(db, project, name).await?.is_some())
}

async fn granted_team<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    name: &str,
) -> Result<Option<TeamId>, DbErr> {
    let Some(team) = ETeam::find().filter(CTeam::Name.eq(name)).one(db).await? else {
        return Ok(None);
    };
    let granted = ETeamProject::find()
        .filter(CTeamProject::Team.eq(team.id))
        .filter(CTeamProject::Project.eq(project))
        .filter(CTeamProject::IncludesUsers.eq(true))
        .one(db)
        .await?
        .is_some();
    Ok(granted.then_some(team.id))
}

async fn team_emails<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    name: &str,
) -> Result<Vec<String>, DbErr> {
    let Some(team) = granted_team(db, project, name).await? else {
        return Ok(Vec::new());
    };
    let users: Vec<UserId> = ETeamUser::find()
        .filter(CTeamUser::Team.eq(team))
        .all(db)
        .await?
        .into_iter()
        .map(|membership| membership.user)
        .collect();

    Ok(EUser::find()
        .filter(CUser::Id.is_in(users))
        .filter(CUser::EmailVerified.eq(true))
        .filter(CUser::Active.eq(true))
        .all(db)
        .await?
        .into_iter()
        .map(|user| user.email)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    #[tokio::test]
    async fn a_team_without_access_drops_out_and_the_rest_still_gets_mail() {
        let (granted, revoked) = (
            MTeam {
                id: TeamId::now_v7(),
                name: "eng".into(),
                ..Default::default()
            },
            MTeam {
                id: TeamId::now_v7(),
                name: "gone".into(),
                ..Default::default()
            },
        );
        let member = UserId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![granted.clone()]])
            .append_query_results([vec![MTeamProject {
                team: granted.id,
                includes_users: true,
                ..Default::default()
            }]])
            .append_query_results([vec![MTeamUser {
                team: granted.id,
                user: member,
                ..Default::default()
            }]])
            .append_query_results([vec![MUser {
                id: member,
                email: "alice@example.com".into(),
                email_verified: true,
                active: true,
                ..Default::default()
            }]])
            .append_query_results([vec![revoked]])
            .append_query_results([Vec::<MTeamProject>::new()])
            .into_connection();

        let recipients = resolve_mail_recipients(
            &db,
            ProjectId::now_v7(),
            &[
                "team:eng".into(),
                "ops@example.com".into(),
                "team:gone".into(),
            ],
        )
        .await
        .expect("resolve");

        assert_eq!(
            recipients,
            vec![
                "alice@example.com".to_string(),
                "ops@example.com".to_string()
            ]
        );
    }
}
