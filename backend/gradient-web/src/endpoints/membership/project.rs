/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{
    Actions, InvitationFields, InvitationTable, MemberFields, MemberTable, NewInvitation, Owner,
    RoleFields, RoleTable, Table,
};
use crate::error::WebResult;
use crate::permissions::{
    PermissionEntry, PermissionMask, available_permissions, is_builtin_role, mask_to_vec,
    mask_with_view_project_permission, parse_permission_list,
};
use crate::refusal::SharedRefusal;
use crate::resource::Resource;
use crate::scope::{Member, ProjectScope, Readable, Requires, RequiresUnmanaged, can};
use gradient_notify::InviteScope;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ConnectionTrait, RelationDef, RelationTrait};
use serde::Serialize;

pub struct ProjectOwner;

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ProjectRoleOwner {
    project: Option<ProjectId>,
}

impl From<Option<ProjectId>> for ProjectRoleOwner {
    fn from(project: Option<ProjectId>) -> Self {
        Self { project }
    }
}

impl Table for EProjectUser {
    type Row = MProjectUser;
    type Active = AProjectUser;
}

impl Table for EProjectInvitation {
    type Row = MProjectInvitation;
    type Active = AProjectInvitation;
}

impl Table for ERole {
    type Row = MRole;
    type Active = ARole;
}

impl MemberTable for EProjectUser {
    type Id = ProjectId;

    fn owner_column() -> CProjectUser {
        CProjectUser::Project
    }

    fn user_column() -> CProjectUser {
        CProjectUser::User
    }

    fn role_column() -> CProjectUser {
        CProjectUser::Role
    }

    fn user_relation() -> RelationDef {
        gradient_entity::project_user::Relation::User.def()
    }

    fn fields(row: &MProjectUser) -> MemberFields {
        MemberFields {
            user: row.user,
            role: row.role,
        }
    }

    fn new(owner: ProjectId, user: UserId, role: RoleId) -> MProjectUser {
        MProjectUser {
            id: ProjectUserId::now_v7(),
            project: owner,
            user,
            role,
        }
    }

    fn assign_role(row: MProjectUser, role: RoleId) -> AProjectUser {
        let mut active: AProjectUser = row.into();
        active.role = Set(role);
        active
    }
}

impl InvitationTable for EProjectInvitation {
    type Id = ProjectId;

    fn owner_column() -> CProjectInvitation {
        CProjectInvitation::Project
    }

    fn user_column() -> CProjectInvitation {
        CProjectInvitation::User
    }

    fn user_relation() -> RelationDef {
        gradient_entity::project_invitation::Relation::User.def()
    }

    fn fields(row: &MProjectInvitation) -> InvitationFields {
        InvitationFields {
            user: row.user,
            role: row.role,
            created_at: row.created_at,
            expires_at: row.expires_at,
        }
    }

    fn new(invitation: NewInvitation<ProjectId>) -> MProjectInvitation {
        MProjectInvitation {
            id: ProjectInvitationId::now_v7(),
            project: invitation.owner,
            user: invitation.user,
            role: invitation.role,
            invited_by: invitation.invited_by,
            token: invitation.token,
            created_at: invitation.created_at,
            expires_at: invitation.expires_at,
        }
    }
}

impl RoleTable for ERole {
    type Id = ProjectId;
    type OwnerField = ProjectRoleOwner;

    fn id_column() -> CRole {
        CRole::Id
    }

    fn name_column() -> CRole {
        CRole::Name
    }

    fn owner_column() -> CRole {
        CRole::Project
    }

    fn fields(row: &MRole) -> RoleFields<ProjectId> {
        RoleFields {
            id: row.id,
            name: row.name.clone(),
            owner: row.project,
            permission: row.permission,
            managed: row.managed,
        }
    }

    fn new(id: RoleId, name: String, owner: ProjectId, permission: PermissionMask) -> MRole {
        MRole {
            id,
            name,
            project: Some(owner),
            permission,
            ..Default::default()
        }
    }

    fn rename(active: &mut ARole, name: String) {
        active.name = Set(name);
    }

    fn set_permission(active: &mut ARole, permission: PermissionMask) {
        active.permission = Set(permission);
    }
}

impl Owner for ProjectOwner {
    type Id = ProjectId;
    type ListMembersScope = ProjectScope<Readable>;
    type ManageMembersScope = ProjectScope<RequiresUnmanaged<can::ManageMembers>>;
    type ListInvitationsScope = ProjectScope<Requires<can::ManageMembers>>;
    type ReadRolesScope = ProjectScope<Member>;
    type ManageRolesScope = ProjectScope<RequiresUnmanaged<can::ManageRoles>>;
    type Members = EProjectUser;
    type Invitations = EProjectInvitation;
    type Roles = ERole;

    const KIND: &'static str = "project";
    const ID_KEY: &'static str = "project_id";
    const RESOURCE: Resource = Resource::Project;
    const MEMBERSHIP: Resource = Resource::ProjectMembership;
    const INVITE_SCOPE: InviteScope = InviteScope::Project;
    const MEMBER_REMOVED: &'static str = "User kicked";
    const ACTIONS: Actions = Actions {
        member_add: Action::ProjectMemberAdd,
        member_role_change: Action::ProjectMemberRoleChange,
        member_remove: Action::ProjectMemberRemove,
        invitation_create: Action::ProjectInvitationCreate,
        invitation_revoke: Action::ProjectInvitationRevoke,
        role_create: Action::ProjectRoleCreate,
        role_update: Action::ProjectRoleUpdate,
        role_delete: Action::ProjectRoleDelete,
    };

    fn event_owner(id: ProjectId) -> EventOwner {
        EventOwner::project(id)
    }

    fn is_builtin_role(role: RoleId) -> bool {
        is_builtin_role(role)
    }

    fn permission_names(mask: PermissionMask) -> Vec<&'static str> {
        mask_to_vec(mask)
            .into_iter()
            .map(|p| p.as_wire_name())
            .collect()
    }

    fn available_permissions() -> Vec<PermissionEntry> {
        available_permissions()
    }

    fn parse_permissions(wire: &[String]) -> WebResult<PermissionMask> {
        Ok(mask_with_view_project_permission(parse_permission_list(
            wire,
            "GET /projects/{project}/roles",
        )?))
    }

    async fn role_granted_to_a_team(db: &impl ConnectionTrait, role: RoleId) -> WebResult<bool> {
        Ok(gradient_db::teams::grants::project_role_granted_to_a_team(db, role).await?)
    }

    async fn lock_for_admin_change(db: &impl ConnectionTrait, owner: ProjectId) -> WebResult<()> {
        gradient_db::projects::lock_for_admin_change(db, owner).await?;
        Ok(())
    }

    async fn ensure_member_removable(
        db: &impl ConnectionTrait,
        owner: ProjectId,
        member: &MProjectUser,
    ) -> WebResult<()> {
        if member.role == BASE_ROLE_ADMIN_ID {
            let admin_count =
                gradient_db::projects::count_members_with_role(db, owner, BASE_ROLE_ADMIN_ID)
                    .await?;
            if admin_count <= 1 {
                return Err(SharedRefusal::LastAdmin(Resource::Project).into());
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::Permission;

    #[test]
    fn custom_roles_always_view_the_project() {
        let mask = ProjectOwner::parse_permissions(&["triggerEvaluation".to_string()]).unwrap();
        assert_eq!(
            mask,
            Permission::TriggerEvaluation.bit() | Permission::ViewProject.bit()
        );
    }
}
