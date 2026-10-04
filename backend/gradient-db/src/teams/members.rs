/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;

use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    QueryFilter,
};
use std::collections::HashSet;

pub async fn sync_group_memberships<C: ConnectionTrait>(
    db: &C,
    user: UserId,
    groups: &[String],
) -> Result<(), DbErr> {
    let oidc_teams = ETeam::find()
        .filter(CTeam::OidcGroup.is_not_null())
        .all(db)
        .await?;
    let wanted: HashSet<TeamId> = oidc_teams
        .iter()
        .filter(|team| {
            team.oidc_group
                .as_ref()
                .is_some_and(|group| groups.contains(group))
        })
        .map(|team| team.id)
        .collect();
    let synced: HashSet<TeamId> = oidc_teams.iter().map(|team| team.id).collect();

    let current = ETeamUser::find()
        .filter(CTeamUser::User.eq(user))
        .all(db)
        .await?;
    let present: HashSet<TeamId> = current.iter().map(|membership| membership.team).collect();

    for membership in current {
        if membership.source == TeamMemberSource::Group
            && synced.contains(&membership.team)
            && !wanted.contains(&membership.team)
        {
            ETeamUser::delete_by_id(membership.id).exec(db).await?;
        }
    }

    for team in wanted.difference(&present) {
        MTeamUser {
            id: TeamUserId::now_v7(),
            team: *team,
            user,
            role: TeamRole::Member,
            source: TeamMemberSource::Group,
        }
        .into_active_model()
        .insert(db)
        .await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    fn team(group: &str) -> MTeam {
        MTeam {
            id: TeamId::now_v7(),
            oidc_group: Some(group.into()),
            ..Default::default()
        }
    }

    fn membership(team: TeamId, user: UserId, source: TeamMemberSource) -> MTeamUser {
        MTeamUser {
            id: TeamUserId::now_v7(),
            team,
            user,
            role: TeamRole::Member,
            source,
        }
    }

    #[tokio::test]
    async fn sign_in_keeps_scim_rows_of_teams_without_an_oidc_group() {
        let user = UserId::now_v7();
        let scim_only = MTeam {
            id: TeamId::now_v7(),
            scim_group: Some("acme".into()),
            ..Default::default()
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MTeam>::new()])
            .append_query_results([vec![membership(
                scim_only.id,
                user,
                TeamMemberSource::Group,
            )]])
            .into_connection();

        sync_group_memberships(&db, user, &[]).await.expect("sync");

        let deleted = db
            .into_transaction_log()
            .iter()
            .flat_map(|t| t.statements().to_vec())
            .any(|s| s.sql.starts_with("DELETE"));
        assert!(!deleted, "a SCIM membership must survive an OIDC sign-in");
    }

    #[tokio::test]
    async fn sign_in_joins_group_teams_and_leaves_lost_ones_but_keeps_manual_rows() {
        let user = UserId::now_v7();
        let (joined, left, manual) = (team("eng"), team("ops"), team("sales"));
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![joined.clone(), left.clone(), manual.clone()]])
            .append_query_results([vec![
                membership(left.id, user, TeamMemberSource::Group),
                membership(manual.id, user, TeamMemberSource::Api),
            ]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![membership(joined.id, user, TeamMemberSource::Group)]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        sync_group_memberships(&db, user, &["eng".to_string()])
            .await
            .expect("sync");

        let statements: Vec<String> = db
            .into_transaction_log()
            .iter()
            .flat_map(|t| t.statements().to_vec())
            .map(|s| s.sql)
            .collect();
        assert_eq!(
            statements
                .iter()
                .filter(|s| s.starts_with("DELETE FROM \"team_user\""))
                .count(),
            1
        );
        assert_eq!(
            statements
                .iter()
                .filter(|s| s.starts_with("INSERT INTO \"team_user\""))
                .count(),
            1
        );
    }
}
