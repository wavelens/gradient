/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;

use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel, JoinType,
    PaginatorTrait, QueryFilter, QuerySelect, RelationTrait,
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
        if membership.source == TeamMemberSource::SignInGroupSync
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
            source: TeamMemberSource::SignInGroupSync,
        }
        .into_active_model()
        .insert(db)
        .await?;
    }

    Ok(())
}

pub async fn membership<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
    user: UserId,
) -> Result<Option<MTeamUser>, DbErr> {
    ETeamUser::find()
        .filter(CTeamUser::Team.eq(team))
        .filter(CTeamUser::User.eq(user))
        .one(db)
        .await
}

pub async fn admin_count<C: ConnectionTrait>(db: &C, team: TeamId) -> Result<u64, DbErr> {
    ETeamUser::find()
        .filter(CTeamUser::Team.eq(team))
        .filter(CTeamUser::Role.eq(TeamRole::Admin))
        .count(db)
        .await
}

pub async fn members_of<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
) -> Result<Vec<(MTeamUser, Option<MUser>)>, DbErr> {
    ETeamUser::find()
        .join(
            JoinType::InnerJoin,
            gradient_entity::team_user::Relation::User.def(),
        )
        .select_also(gradient_entity::user::Entity)
        .filter(CTeamUser::Team.eq(team))
        .all(db)
        .await
}

pub async fn teams_of_user<C: ConnectionTrait>(
    db: &C,
    user: UserId,
) -> Result<Vec<(MTeamUser, Option<MTeam>)>, DbErr> {
    ETeamUser::find()
        .join(
            JoinType::InnerJoin,
            gradient_entity::team_user::Relation::Team.def(),
        )
        .select_also(gradient_entity::team::Entity)
        .filter(CTeamUser::User.eq(user))
        .all(db)
        .await
}

pub async fn add_member<C: ConnectionTrait>(
    db: &C,
    membership: MTeamUser,
) -> Result<MTeamUser, DbErr> {
    membership.into_active_model().insert(db).await
}

pub async fn add_team_user_if_absent<C: ConnectionTrait>(
    db: &C,
    membership: MTeamUser,
) -> Result<(), DbErr> {
    ETeamUser::insert(membership.into_active_model())
        .on_conflict(
            OnConflict::columns([CTeamUser::Team, CTeamUser::User])
                .do_nothing()
                .to_owned(),
        )
        .exec_without_returning(db)
        .await?;
    Ok(())
}

pub async fn update_member<C: ConnectionTrait>(
    db: &C,
    membership: ATeamUser,
) -> Result<MTeamUser, DbErr> {
    membership.update(db).await
}

pub async fn remove_member<C: ConnectionTrait>(db: &C, membership: MTeamUser) -> Result<(), DbErr> {
    membership.into_active_model().delete(db).await?;
    Ok(())
}

pub async fn memberships_of<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
) -> Result<Vec<MTeamUser>, DbErr> {
    ETeamUser::find()
        .filter(CTeamUser::Team.eq(team))
        .all(db)
        .await
}

pub async fn scim_memberships_of_team<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
) -> Result<Vec<MTeamUser>, DbErr> {
    ETeamUser::find()
        .join(
            JoinType::InnerJoin,
            gradient_entity::team_user::Relation::User.def(),
        )
        .filter(CTeamUser::Team.eq(team))
        .filter(CUser::AccountOrigin.eq(AccountOrigin::ScimProvisionedByIdentityProvider))
        .all(db)
        .await
}

pub async fn remove_group_member<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
    user: UserId,
) -> Result<(), DbErr> {
    ETeamUser::delete_many()
        .filter(CTeamUser::Team.eq(team))
        .filter(CTeamUser::User.eq(user))
        .filter(CTeamUser::Source.eq(TeamMemberSource::ScimGroup))
        .exec(db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::{all_statements, mock_db};
    use sea_orm::MockExecResult;

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
    async fn team_membership_group_origin_migration_splits_scim_group_and_sign_in_group_sync() {
        use gradient_migration::{Migrator, MigratorTrait};
        use sea_orm_migration::SchemaManager;

        let db = gradient_fixtures::database::TestDatabase::create().await;
        let conn = db.connection();
        let mut users = Vec::new();
        for (username, origin) in [
            (
                "signed-in",
                AccountOrigin::CreatedOnFirstSignInWithExternalLoginProvider,
            ),
            ("scim", AccountOrigin::ScimProvisionedByIdentityProvider),
        ] {
            let user = crate::rows::insert::<EUser, _>(
                &conn,
                MUser {
                    id: UserId::now_v7(),
                    username: username.into(),
                    name: username.into(),
                    email: format!("{username}@x.io"),
                    account_origin: origin,
                    active: true,
                    ..Default::default()
                },
            )
            .await
            .expect("the account is written");
            users.push(user.id);
        }
        let (signed_in, scim) = (users[0], users[1]);
        let mut teams = Vec::new();
        for (name, scim_group) in [
            ("oidc-only", None),
            ("scim-backed", Some("acme")),
            ("manual", Some("ops")),
        ] {
            let team = crate::rows::insert::<ETeam, _>(
                &conn,
                MTeam {
                    id: TeamId::now_v7(),
                    name: name.into(),
                    display_name: name.into(),
                    oidc_group: Some(format!("{name}-group")),
                    scim_group: scim_group.map(str::to_owned),
                    ..Default::default()
                },
            )
            .await
            .expect("the team is written");
            teams.push(team.id);
        }
        let (oidc_only, scim_backed, manual) = (teams[0], teams[1], teams[2]);
        let migration = Migrator::migrations()
            .into_iter()
            .find(|m| m.name().starts_with("m20261223_000000_"))
            .expect("the migration is registered");
        migration
            .down(&SchemaManager::new(&conn))
            .await
            .expect("down runs");

        let cases = [
            (
                oidc_only,
                signed_in,
                TeamMemberSource::ScimGroup,
                TeamMemberSource::SignInGroupSync,
            ),
            (
                scim_backed,
                signed_in,
                TeamMemberSource::ScimGroup,
                TeamMemberSource::SignInGroupSync,
            ),
            (
                oidc_only,
                scim,
                TeamMemberSource::ScimGroup,
                TeamMemberSource::SignInGroupSync,
            ),
            (
                scim_backed,
                scim,
                TeamMemberSource::ScimGroup,
                TeamMemberSource::ScimGroup,
            ),
            (
                manual,
                signed_in,
                TeamMemberSource::Api,
                TeamMemberSource::Api,
            ),
            (
                manual,
                scim,
                TeamMemberSource::State,
                TeamMemberSource::State,
            ),
        ];
        let mut expected = Vec::new();
        for (team, user, source, migrated) in cases {
            let row = crate::rows::insert::<ETeamUser, _>(&conn, membership(team, user, source))
                .await
                .expect("the membership is written");
            expected.push((row.id, migrated));
        }

        migration
            .up(&SchemaManager::new(&conn))
            .await
            .expect("up runs");

        for (id, migrated) in expected {
            let stored = db
                .find::<ETeamUser>(id)
                .await
                .expect("the membership exists");
            assert_eq!(stored.source, migrated, "{stored:?}");
        }
        db.finish().await;
    }

    #[tokio::test]
    async fn test_m20261223_old_server_membership_source_2_becomes_source_3_unless_scim_provisioned_and_team_has_scim_group()
     {
        let db = gradient_fixtures::database::TestDatabase::create().await;
        let conn = db.connection();
        let mut users = Vec::new();
        for (username, origin) in [
            (
                "signed-in",
                AccountOrigin::CreatedOnFirstSignInWithExternalLoginProvider,
            ),
            ("scim", AccountOrigin::ScimProvisionedByIdentityProvider),
        ] {
            let user = crate::rows::insert::<EUser, _>(
                &conn,
                MUser {
                    id: UserId::now_v7(),
                    username: username.into(),
                    name: username.into(),
                    email: format!("{username}@x.io"),
                    account_origin: origin,
                    active: true,
                    ..Default::default()
                },
            )
            .await
            .expect("the account is written");
            users.push(user.id);
        }
        let (signed_in, scim) = (users[0], users[1]);
        let mut teams = Vec::new();
        for (name, scim_group) in [("oidc-only", None), ("scim-backed", Some("acme"))] {
            let team = crate::rows::insert::<ETeam, _>(
                &conn,
                MTeam {
                    id: TeamId::now_v7(),
                    name: name.into(),
                    display_name: name.into(),
                    oidc_group: Some(format!("{name}-group")),
                    scim_group: scim_group.map(str::to_owned),
                    ..Default::default()
                },
            )
            .await
            .expect("the team is written");
            teams.push(team.id);
        }
        let (oidc_only, scim_backed) = (teams[0], teams[1]);

        let mut expected = Vec::new();
        for (team, user, migrated) in [
            (oidc_only, scim, TeamMemberSource::SignInGroupSync),
            (scim_backed, signed_in, TeamMemberSource::SignInGroupSync),
            (scim_backed, scim, TeamMemberSource::ScimGroup),
        ] {
            let id = TeamUserId::now_v7();
            db.execute(&format!(
                "INSERT INTO team_user (id, team, \"user\", role, source) \
                 VALUES ('{id}', '{team}', '{user}', 1, 2);"
            ))
            .await;
            expected.push((id, migrated));
        }
        let updated = TeamUserId::now_v7();
        db.execute(&format!(
            "INSERT INTO team_user (id, team, \"user\", role, source) \
             VALUES ('{updated}', '{oidc_only}', '{signed_in}', 1, 0); \
             UPDATE team_user SET source = 2 WHERE id = '{updated}';"
        ))
        .await;
        expected.push((updated, TeamMemberSource::SignInGroupSync));

        for (id, migrated) in expected {
            let stored = db
                .find::<ETeamUser>(id)
                .await
                .expect("the membership exists");
            assert_eq!(stored.source, migrated, "{stored:?}");
        }
        db.finish().await;
    }

    #[tokio::test]
    async fn test_team_user_trigger_keeps_source_2_when_role_changes_after_scim_group_cleared() {
        let db = gradient_fixtures::database::TestDatabase::create().await;
        let conn = db.connection();
        let user = crate::rows::insert::<EUser, _>(
            &conn,
            MUser {
                id: UserId::now_v7(),
                username: "scim".into(),
                name: "scim".into(),
                email: "scim@x.io".into(),
                account_origin: AccountOrigin::ScimProvisionedByIdentityProvider,
                active: true,
                ..Default::default()
            },
        )
        .await
        .expect("the account is written");
        let team = crate::rows::insert::<ETeam, _>(
            &conn,
            MTeam {
                id: TeamId::now_v7(),
                name: "scim-backed".into(),
                display_name: "scim-backed".into(),
                scim_group: Some("acme".into()),
                ..Default::default()
            },
        )
        .await
        .expect("the team is written");
        let membership = TeamUserId::now_v7();
        db.execute(&format!(
            "INSERT INTO team_user (id, team, \"user\", role, source) \
             VALUES ('{membership}', '{team}', '{user}', 1, 2); \
             UPDATE team SET scim_group = NULL WHERE id = '{team}'; \
             UPDATE team_user SET role = 0 WHERE id = '{membership}';",
            team = team.id,
            user = user.id,
        ))
        .await;

        let stored = db
            .find::<ETeamUser>(membership)
            .await
            .expect("the membership exists");
        assert_eq!(stored.source, TeamMemberSource::ScimGroup, "{stored:?}");
        db.finish().await;
    }

    #[tokio::test]
    async fn sign_in_keeps_scim_rows_of_teams_without_an_oidc_group() {
        let user = UserId::now_v7();
        let scim_only = MTeam {
            id: TeamId::now_v7(),
            scim_group: Some("acme".into()),
            ..Default::default()
        };
        let db = mock_db()
            .append_query_results([Vec::<MTeam>::new()])
            .append_query_results([vec![membership(
                scim_only.id,
                user,
                TeamMemberSource::ScimGroup,
            )]])
            .into_connection();

        sync_group_memberships(&db, user, &[]).await.expect("sync");

        let deleted = all_statements(db.into_transaction_log())
            .into_iter()
            .any(|s| s.sql.starts_with("DELETE"));
        assert!(!deleted, "a SCIM membership must survive an OIDC sign-in");
    }

    #[tokio::test]
    async fn sign_in_without_the_group_keeps_scim_rows_of_teams_with_an_oidc_group() {
        let user = UserId::now_v7();
        let both = MTeam {
            scim_group: Some("acme".into()),
            ..team("eng")
        };
        let db = mock_db()
            .append_query_results([vec![both.clone()]])
            .append_query_results([vec![membership(both.id, user, TeamMemberSource::ScimGroup)]])
            .into_connection();

        sync_group_memberships(&db, user, &[]).await.expect("sync");

        let deleted = all_statements(db.into_transaction_log())
            .into_iter()
            .any(|s| s.sql.starts_with("DELETE"));
        assert!(!deleted, "a SCIM membership must survive an OIDC sign-in");
    }

    #[tokio::test]
    async fn sign_in_joins_group_teams_and_leaves_lost_ones_but_keeps_manual_rows() {
        let user = UserId::now_v7();
        let (joined, left, manual) = (team("eng"), team("ops"), team("sales"));
        let db = mock_db()
            .append_query_results([vec![joined.clone(), left.clone(), manual.clone()]])
            .append_query_results([vec![
                membership(left.id, user, TeamMemberSource::SignInGroupSync),
                membership(manual.id, user, TeamMemberSource::Api),
            ]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![membership(
                joined.id,
                user,
                TeamMemberSource::SignInGroupSync,
            )]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        sync_group_memberships(&db, user, &["eng".to_string()])
            .await
            .expect("sync");

        let statements: Vec<String> = all_statements(db.into_transaction_log())
            .into_iter()
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
