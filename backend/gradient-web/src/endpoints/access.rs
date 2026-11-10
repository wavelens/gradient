/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::api_spec::{CachePath, ProjectPath, UserPath};
use crate::endpoints::membership::cache::CacheOwner;
use crate::endpoints::membership::project::ProjectOwner;
use crate::endpoints::membership::{
    MemberTable, Owner, OwnerScope, RoleTable, TeamMembershipRoleGrant, find_user_by_username,
};
use crate::error::{WebError, WebResult, require_superuser};
use crate::helpers::ok_json;
use crate::permissions::PermissionMask;
use crate::scope::{CacheScope, ProjectScope, Readable, Requires, can};
use axum::extract::State;
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::WebDb;
use gradient_db::access::{DirectMembershipsOfUser, RevokedUserAccessCounts};
use gradient_db::lookup::by_ids;
use gradient_types::*;
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct TeamNameAndDisplayName {
    pub name: String,
    pub display_name: String,
}

impl From<&MTeam> for TeamNameAndDisplayName {
    fn from(team: &MTeam) -> Self {
        Self {
            name: team.name.clone(),
            display_name: team.display_name.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccessReason {
    Direct,
    Team,
    Owner,
    Superuser,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct PersonAccessReason {
    pub kind: AccessReason,
    pub team: Option<TeamNameAndDisplayName>,
    pub role: Option<String>,
    pub applies: bool,
    pub managed: bool,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct PersonWithAccessToProjectOrCache {
    pub user: String,
    pub user_display_name: String,
    pub role: Option<String>,
    pub permissions: Vec<String>,
    pub reasons: Vec<PersonAccessReason>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct UserAccessibleProjectOrCacheRow {
    pub name: String,
    pub display_name: String,
    pub owner_team: Option<TeamNameAndDisplayName>,
    pub role: Option<String>,
    pub reasons: Vec<PersonAccessReason>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct UserTeamMembership {
    pub name: String,
    pub display_name: String,
    pub role: TeamRole,
    pub source: TeamMemberSource,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct UserAccessSummary {
    pub superuser: bool,
    pub projects: Vec<UserAccessibleProjectOrCacheRow>,
    pub caches: Vec<UserAccessibleProjectOrCacheRow>,
    pub teams: Vec<UserTeamMembership>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RoleAccess {
    pub name: String,
    pub rank: u8,
    pub permission: PermissionMask,
}

#[derive(Clone, Debug)]
pub(crate) struct TeamGrantReachingPerson {
    pub team: TeamNameAndDisplayName,
    pub role: RoleAccess,
    pub source: TeamGrantSource,
}

pub(crate) fn compute_person_access_reasons(
    direct: Option<&RoleAccess>,
    direct_managed: bool,
    teams: &[TeamGrantReachingPerson],
    superuser: bool,
) -> Vec<PersonAccessReason> {
    let direct_reason = direct.map(|role| PersonAccessReason {
        kind: AccessReason::Direct,
        team: None,
        role: Some(role.name.clone()),
        applies: true,
        managed: direct_managed,
    });
    let team_reasons = teams.iter().map(|grant| PersonAccessReason {
        kind: if grant.source == TeamGrantSource::Owner {
            AccessReason::Owner
        } else {
            AccessReason::Team
        },
        team: Some(grant.team.clone()),
        role: Some(grant.role.name.clone()),
        applies: direct.is_none(),
        managed: grant.source == TeamGrantSource::State,
    });
    let superuser_reason = superuser.then_some(PersonAccessReason {
        kind: AccessReason::Superuser,
        team: None,
        role: None,
        applies: true,
        managed: false,
    });
    direct_reason
        .into_iter()
        .chain(team_reasons)
        .chain(superuser_reason)
        .collect()
}

pub(crate) fn person_role_and_permission_mask_for_project_or_cache(
    direct: Option<&RoleAccess>,
    teams: &[TeamGrantReachingPerson],
) -> (Option<String>, PermissionMask) {
    match direct {
        Some(role) => (Some(role.name.clone()), role.permission),
        None => (
            teams
                .iter()
                .max_by_key(|grant| grant.role.rank)
                .map(|grant| grant.role.name.clone()),
            teams
                .iter()
                .fold(0, |mask, grant| mask | grant.role.permission),
        ),
    }
}

pub(crate) async fn team_names<C: ConnectionTrait>(
    db: &C,
    teams: impl IntoIterator<Item = TeamId>,
) -> WebResult<HashMap<TeamId, TeamNameAndDisplayName>> {
    let ids: Vec<TeamId> = teams.into_iter().collect();
    Ok(by_ids::<ETeam, _>(db, &ids)
        .await?
        .iter()
        .map(|(id, team)| (*id, team.into()))
        .collect())
}

async fn role_access<O: Owner>(
    db: &WebDb,
    roles: impl IntoIterator<Item = RoleId>,
) -> WebResult<HashMap<RoleId, RoleAccess>> {
    let ids: Vec<RoleId> = roles.into_iter().collect();
    Ok(by_ids::<O::Roles, _>(db, &ids)
        .await?
        .into_iter()
        .map(|(id, row)| {
            let fields = <O::Roles as RoleTable>::fields(&row);
            (
                id,
                RoleAccess {
                    name: fields.name,
                    rank: O::role_rank(id),
                    permission: fields.permission,
                },
            )
        })
        .collect())
}

fn grants_reaching<I: PartialEq>(
    grants: &[TeamMembershipRoleGrant<I>],
    owner: I,
    user: UserId,
    teams: &HashMap<TeamId, TeamNameAndDisplayName>,
    roles: &HashMap<RoleId, RoleAccess>,
) -> Vec<TeamGrantReachingPerson> {
    grants
        .iter()
        .filter(|grant| grant.owner == owner && grant.user == user)
        .filter_map(|grant| {
            Some(TeamGrantReachingPerson {
                team: teams.get(&grant.team)?.clone(),
                role: roles.get(&grant.role)?.clone(),
                source: grant.source,
            })
        })
        .collect()
}

pub(crate) async fn people_with_access<O: Owner>(
    db: &WebDb,
    owner: O::Id,
    managed: bool,
) -> WebResult<Vec<PersonWithAccessToProjectOrCache>> {
    let direct: HashMap<UserId, RoleId> = gradient_db::memberships::with_users::<O::Members, _>(
        db,
        <O::Members as MemberTable>::user_relation(),
        <O::Members as MemberTable>::owner_column(),
        owner,
    )
    .await?
    .iter()
    .map(|(row, _)| {
        let fields = <O::Members as MemberTable>::fields(row);
        (fields.user, fields.role)
    })
    .collect();
    let grants = O::list_team_grants_with_members(db, owner).await?;
    let superusers = gradient_db::access::superusers(db).await?;

    let user_ids: HashSet<UserId> = direct
        .keys()
        .copied()
        .chain(grants.iter().map(|grant| grant.user))
        .chain(superusers.iter().map(|user| user.id))
        .collect();
    let mut users =
        gradient_db::users::with_ids(db, &user_ids.into_iter().collect::<Vec<_>>()).await?;
    users.sort_by(|a, b| a.username.cmp(&b.username));
    let roles = role_access::<O>(
        db,
        direct
            .values()
            .copied()
            .chain(grants.iter().map(|grant| grant.role)),
    )
    .await?;
    let teams = team_names(db, grants.iter().map(|grant| grant.team)).await?;

    Ok(users
        .into_iter()
        .map(|user| {
            let direct_role = direct.get(&user.id).and_then(|role| roles.get(role));
            let team_grants = grants_reaching(&grants, owner, user.id, &teams, &roles);
            let (role, mask) =
                person_role_and_permission_mask_for_project_or_cache(direct_role, &team_grants);
            PersonWithAccessToProjectOrCache {
                reasons: compute_person_access_reasons(
                    direct_role,
                    managed,
                    &team_grants,
                    user.superuser,
                ),
                user: user.username,
                user_display_name: user.name,
                role,
                permissions: O::permission_names(mask)
                    .into_iter()
                    .map(String::from)
                    .collect(),
            }
        })
        .collect())
}

async fn reachable_by_user<O: Owner>(
    db: &WebDb,
    user: &MUser,
    memberships: &DirectMembershipsOfUser,
) -> WebResult<Vec<UserAccessibleProjectOrCacheRow>> {
    let direct: HashMap<O::Id, RoleId> = O::select_direct_memberships_from_user(memberships)
        .into_iter()
        .collect();
    let grants = O::list_team_grants_reaching_person_through_team_memberships(db, user.id).await?;
    let owners: Vec<O::Id> = direct
        .keys()
        .copied()
        .chain(grants.iter().map(|grant| grant.owner))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let labels =
        O::load_project_name_display_name_owner_team_and_managed_flag_by_ids(db, &owners).await?;
    let roles = role_access::<O>(
        db,
        direct
            .values()
            .copied()
            .chain(grants.iter().map(|grant| grant.role)),
    )
    .await?;
    let teams = team_names(
        db,
        grants
            .iter()
            .map(|grant| grant.team)
            .chain(labels.values().filter_map(|label| label.owner_team)),
    )
    .await?;

    let mut rows: Vec<UserAccessibleProjectOrCacheRow> = labels
        .into_iter()
        .map(|(owner, label)| {
            let direct_role = direct.get(&owner).and_then(|role| roles.get(role));
            let team_grants = grants_reaching(&grants, owner, user.id, &teams, &roles);
            let (role, _) =
                person_role_and_permission_mask_for_project_or_cache(direct_role, &team_grants);
            UserAccessibleProjectOrCacheRow {
                reasons: compute_person_access_reasons(
                    direct_role,
                    label.managed,
                    &team_grants,
                    user.superuser,
                ),
                owner_team: label.owner_team.and_then(|team| teams.get(&team).cloned()),
                name: label.name,
                display_name: label.display_name,
                role,
            }
        })
        .collect();
    rows.sort_by(|a, b| (&a.display_name, &a.name).cmp(&(&b.display_name, &b.name)));
    Ok(rows)
}

async fn user_access_summary(db: &WebDb, user: &MUser) -> WebResult<UserAccessSummary> {
    let memberships = gradient_db::access::direct_memberships_of_user(db, user.id).await?;
    let mut teams: Vec<UserTeamMembership> =
        gradient_db::teams::members::teams_of_user(db, user.id)
            .await?
            .into_iter()
            .filter_map(|(membership, team)| {
                let team = team?;
                Some(UserTeamMembership {
                    name: team.name,
                    display_name: team.display_name,
                    role: membership.role,
                    source: membership.source,
                })
            })
            .collect();
    teams.sort_by(|a, b| (&a.display_name, &a.name).cmp(&(&b.display_name, &b.name)));
    Ok(UserAccessSummary {
        superuser: user.superuser,
        projects: reachable_by_user::<ProjectOwner>(db, user, &memberships).await?,
        caches: reachable_by_user::<CacheOwner>(db, user, &memberships).await?,
        teams,
    })
}

#[utoipa::path(
    get,
    path = "/projects/{project}/access",
    operation_id = "listProjectAccess",
    tag = "projects",
    summary = "List project access",
    description = "List everyone who can access the project. Return the effective role, permissions, and reason (direct membership, team grant, owner team, or instance admin). Use direct membership to set the role. Apply a team grant only to people who don't have direct membership.",
    params(ProjectPath),
    responses((status = OK, description = "One row per person", body = Vec<PersonWithAccessToProjectOrCache>), WebError)
)]
pub async fn get_project_access(
    scope: ProjectScope<Readable>,
) -> WebResult<Json<BaseResponse<Vec<PersonWithAccessToProjectOrCache>>>> {
    Ok(ok_json(
        people_with_access::<ProjectOwner>(&scope.db().web_db, scope.id(), scope.managed()).await?,
    ))
}

#[utoipa::path(
    get,
    path = "/caches/{cache}/access",
    operation_id = "listCacheAccess",
    tag = "caches",
    summary = "List cache access",
    description = "List everyone with access to the cache. Include effective role, permissions, and reason (direct membership, team grant, or instance admin status). Direct memberships assign the role. Team grants apply only to people without direct membership.",
    params(CachePath),
    responses((status = OK, description = "One row per person", body = Vec<PersonWithAccessToProjectOrCache>), WebError)
)]
pub async fn get_cache_access(
    scope: CacheScope<Requires<can::ViewCache>>,
) -> WebResult<Json<BaseResponse<Vec<PersonWithAccessToProjectOrCache>>>> {
    Ok(ok_json(
        people_with_access::<CacheOwner>(&scope.db().web_db, scope.id(), scope.managed()).await?,
    ))
}

#[utoipa::path(
    get,
    path = "/user/access",
    operation_id = "getUserAccess",
    tag = "user",
    summary = "Get own access",
    description = "List all projects, caches, and teams that the signed-in user can access, with the reason for each.",
    responses((status = OK, description = "Access summary", body = UserAccessSummary), WebError)
)]
pub async fn get_own_access(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<UserAccessSummary>>> {
    Ok(ok_json(user_access_summary(&state.web_db, &user).await?))
}

#[utoipa::path(
    get,
    path = "/users/{user}/access",
    operation_id = "getAnyUserAccess",
    tag = "user",
    summary = "Get user access",
    description = "List every project, cache and team that the user can access. Include the reason for each. Instance admins only.",
    params(UserPath),
    responses((status = OK, description = "Access summary", body = UserAccessSummary), WebError)
)]
pub async fn get_user_access(
    state: State<Arc<ServerState>>,
    Extension(caller): Extension<MUser>,
    axum::extract::Path(username): axum::extract::Path<String>,
) -> WebResult<Json<BaseResponse<UserAccessSummary>>> {
    require_superuser(&caller)?;
    let user = find_user_by_username(&state.web_db, &username).await?;
    Ok(ok_json(user_access_summary(&state.web_db, &user).await?))
}

#[utoipa::path(
    post,
    path = "/users/{user}/revoke-access",
    operation_id = "revokeUserAccess",
    tag = "user",
    summary = "Revoke user access",
    description = "Remove direct membership in the user's projects and caches, team memberships, and pending invitations in one transaction. Rows from the server configuration and from groups of the identity provider remain. A rule to keep the last admin does not apply here. A project or cache can have no admin. Only instance administrators.",
    params(UserPath),
    responses((status = OK, description = "Number of removed rows", body = RevokedUserAccessCounts), WebError)
)]
pub async fn post_revoke_user_access(
    state: State<Arc<ServerState>>,
    Extension(caller): Extension<MUser>,
    axum::extract::Path(username): axum::extract::Path<String>,
) -> WebResult<Json<BaseResponse<RevokedUserAccessCounts>>> {
    require_superuser(&caller)?;
    let user = find_user_by_username(&state.web_db, &username).await?;
    let tx = state.web_db.inner().begin().await?;
    let revoked =
        gradient_db::access::delete_user_memberships_and_pending_invitations_skipping_configured_rows(
            &tx, user.id,
        )
        .await?;
    tx.commit().await?;
    Ok(ok_json(revoked))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role(name: &str, rank: u8, permission: PermissionMask) -> RoleAccess {
        RoleAccess {
            name: name.into(),
            rank,
            permission,
        }
    }

    fn grant(team: &str, role: RoleAccess, source: TeamGrantSource) -> TeamGrantReachingPerson {
        TeamGrantReachingPerson {
            team: TeamNameAndDisplayName {
                name: team.into(),
                display_name: team.into(),
            },
            role,
            source,
        }
    }

    #[test]
    fn a_direct_role_overrides_a_stronger_team_role() {
        let view = role("View", 0, 0b001);
        let teams = [grant(
            "alice-team",
            role("Admin", 3, 0b111),
            TeamGrantSource::Owner,
        )];

        assert_eq!(
            person_role_and_permission_mask_for_project_or_cache(Some(&view), &teams),
            (Some("View".into()), 0b001)
        );
        let reasons = compute_person_access_reasons(Some(&view), false, &teams, false);
        assert_eq!(
            reasons
                .iter()
                .map(|r| (r.kind, r.applies))
                .collect::<Vec<_>>(),
            [(AccessReason::Direct, true), (AccessReason::Owner, false)]
        );
    }

    #[test]
    fn without_a_direct_row_the_strongest_team_role_and_all_team_permissions_apply() {
        let teams = [
            grant("a", role("Write", 2, 0b011), TeamGrantSource::Manual),
            grant("b", role("Custom", 1, 0b100), TeamGrantSource::State),
        ];

        assert_eq!(
            person_role_and_permission_mask_for_project_or_cache(None, &teams),
            (Some("Write".into()), 0b111)
        );
        let reasons = compute_person_access_reasons(None, false, &teams, true);
        assert_eq!(
            reasons
                .iter()
                .map(|r| (r.kind, r.applies, r.managed))
                .collect::<Vec<_>>(),
            [
                (AccessReason::Team, true, false),
                (AccessReason::Team, true, true),
                (AccessReason::Superuser, true, false),
            ]
        );
    }
}
