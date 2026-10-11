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
    PermissionEntry, PermissionMask, available_cache_permissions, cache_mask_to_wire,
    is_builtin_cache_role, parse_cache_permission_list,
};
use crate::refusal::SharedRefusal;
use crate::resource::Resource;
use crate::scope::{CacheScope, Member, Requires, RequiresUnmanaged, can};
use gradient_notify::InviteScope;
use gradient_types::consts::BASE_CACHE_ROLE_ADMIN_ID;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ConnectionTrait, RelationDef, RelationTrait};
use serde::Serialize;

pub struct CacheOwner;

#[derive(Serialize, Debug)]
pub struct CacheRoleOwner {
    cache: Option<CacheId>,
}

impl Table for ECacheUser {
    type Row = MCacheUser;
    type Active = ACacheUser;
}

impl Table for ECacheInvitation {
    type Row = MCacheInvitation;
    type Active = ACacheInvitation;
}

impl Table for ECacheRole {
    type Row = MCacheRole;
    type Active = ACacheRole;
}

impl MemberTable for ECacheUser {
    type Id = CacheId;

    fn owner_column() -> CCacheUser {
        CCacheUser::Cache
    }

    fn user_column() -> CCacheUser {
        CCacheUser::User
    }

    fn role_column() -> CCacheUser {
        CCacheUser::Role
    }

    fn user_relation() -> RelationDef {
        gradient_entity::cache_user::Relation::User.def()
    }

    fn fields(row: &MCacheUser) -> MemberFields {
        MemberFields {
            user: row.user,
            role: row.role,
        }
    }

    fn new(owner: CacheId, user: UserId, role: RoleId) -> MCacheUser {
        MCacheUser {
            id: CacheUserId::now_v7(),
            cache: owner,
            user,
            role,
        }
    }

    fn assign_role(row: MCacheUser, role: RoleId) -> ACacheUser {
        let mut active: ACacheUser = row.into();
        active.role = Set(role);
        active
    }
}

impl InvitationTable for ECacheInvitation {
    type Id = CacheId;

    fn owner_column() -> CCacheInvitation {
        CCacheInvitation::Cache
    }

    fn user_column() -> CCacheInvitation {
        CCacheInvitation::User
    }

    fn user_relation() -> RelationDef {
        gradient_entity::cache_invitation::Relation::User.def()
    }

    fn fields(row: &MCacheInvitation) -> InvitationFields {
        InvitationFields {
            user: row.user,
            role: row.role,
            created_at: row.created_at,
            expires_at: row.expires_at,
        }
    }

    fn new(invitation: NewInvitation<CacheId>) -> MCacheInvitation {
        MCacheInvitation {
            id: CacheInvitationId::now_v7(),
            cache: invitation.owner,
            user: invitation.user,
            role: invitation.role,
            invited_by: invitation.invited_by,
            token: invitation.token,
            created_at: invitation.created_at,
            expires_at: invitation.expires_at,
        }
    }
}

impl RoleTable for ECacheRole {
    type Id = CacheId;
    type OwnerField = CacheRoleOwner;

    fn id_column() -> CCacheRole {
        CCacheRole::Id
    }

    fn name_column() -> CCacheRole {
        CCacheRole::Name
    }

    fn owner_column() -> CCacheRole {
        CCacheRole::Cache
    }

    fn fields(row: &MCacheRole) -> RoleFields<CacheId> {
        RoleFields {
            id: row.id,
            name: row.name.clone(),
            owner: row.cache,
            permission: row.permission,
            managed: row.managed,
        }
    }

    fn owner_field(owner: Option<CacheId>) -> CacheRoleOwner {
        CacheRoleOwner { cache: owner }
    }

    fn new(id: RoleId, name: String, owner: CacheId, permission: PermissionMask) -> MCacheRole {
        MCacheRole {
            id,
            name,
            cache: Some(owner),
            permission,
            ..Default::default()
        }
    }

    fn rename(active: &mut ACacheRole, name: String) {
        active.name = Set(name);
    }

    fn set_permission(active: &mut ACacheRole, permission: PermissionMask) {
        active.permission = Set(permission);
    }
}

impl Owner for CacheOwner {
    type Id = CacheId;
    type ListMembersScope = CacheScope<Requires<can::ViewCache>>;
    type ManageMembersScope = CacheScope<RequiresUnmanaged<can::ManageCacheMembers>>;
    type ListInvitationsScope = CacheScope<Requires<can::ManageCacheMembers>>;
    type ReadRolesScope = CacheScope<Member>;
    type ManageRolesScope = CacheScope<RequiresUnmanaged<can::ManageCacheRoles>>;
    type Members = ECacheUser;
    type Invitations = ECacheInvitation;
    type Roles = ECacheRole;

    const KIND: &'static str = "cache";
    const ID_KEY: &'static str = "cache_id";
    const RESOURCE: Resource = Resource::Cache;
    const MEMBERSHIP: Resource = Resource::CacheMembership;
    const INVITE_SCOPE: InviteScope = InviteScope::Cache;
    const MEMBER_REMOVED: &'static str = "User removed";
    const ACTIONS: Actions = Actions {
        member_add: Action::CacheMemberCreate,
        member_role_change: Action::CacheMemberUpdate,
        member_remove: Action::CacheMemberDelete,
        invitation_create: Action::CacheInvitationCreate,
        invitation_revoke: Action::CacheInvitationRevoke,
        role_create: Action::CacheRoleCreate,
        role_update: Action::CacheRoleUpdate,
        role_delete: Action::CacheRoleDelete,
    };

    fn event_owner(id: CacheId) -> EventOwner {
        EventOwner::cache(id)
    }

    fn is_builtin_role(role: RoleId) -> bool {
        is_builtin_cache_role(role)
    }

    fn permission_names(mask: PermissionMask) -> Vec<&'static str> {
        cache_mask_to_wire(mask)
    }

    fn available_permissions() -> Vec<PermissionEntry> {
        available_cache_permissions()
    }

    fn parse_permissions(wire: &[String]) -> WebResult<PermissionMask> {
        parse_cache_permission_list(wire, "GET /caches/{cache}/roles")
    }

    async fn role_granted_to_a_team(db: &impl ConnectionTrait, role: RoleId) -> WebResult<bool> {
        Ok(gradient_db::teams::grants::cache_role_granted_to_a_team(db, role).await?)
    }

    async fn lock_for_admin_change(db: &impl ConnectionTrait, owner: CacheId) -> WebResult<()> {
        gradient_db::caches::lock_for_admin_change(db, owner).await?;
        Ok(())
    }

    async fn ensure_member_removable(
        db: &impl ConnectionTrait,
        owner: CacheId,
        member: &MCacheUser,
    ) -> WebResult<()> {
        if member.role == BASE_CACHE_ROLE_ADMIN_ID {
            let admin_count =
                gradient_db::caches::members::count_with_role(db, owner, BASE_CACHE_ROLE_ADMIN_ID)
                    .await?;
            if admin_count <= 1 {
                return Err(SharedRefusal::LastAdmin(Resource::Cache).into());
            }
        }

        Ok(())
    }
}
