/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

macro_rules! owner_endpoints {
    (
        owner: $owner:tt,
        params: $params:tt,
        role_owner: $role_owner:tt,
        tag: $tag:tt,
        $($op:ident: { $($fields:tt)* }),* $(,)?
    ) => {
        use crate::audit::Audit;
        use crate::endpoints::membership::invitations::{self, PendingInvitationItem, RevokeInvitationRequest};
        use crate::endpoints::membership::members::{self, AddMemberRequest, MemberItem, RemoveMemberRequest};
        use crate::endpoints::membership::roles::{
            self, CreateRoleRequest, PatchRoleRequest, RoleListResponse, RoleResponse,
        };
        use crate::endpoints::membership::Owner;
        use crate::error::{WebError, WebResult};
        use crate::json_body::JsonBody;
        use axum::Json;
        use axum::extract::State;
        use gradient_core::ServerState;
        use gradient_types::{BaseResponse, RoleId};
        use std::sync::Arc;

        $( owner_endpoint!($op, $owner, $params, $role_owner, $tag, { $($fields)* }); )*
    };
}

macro_rules! owner_endpoint {
    (list_members, $owner:tt, $params:tt, $role_owner:tt, $tag:tt, {
        path: $path:tt, id: $id:tt, summary: $summary:tt,
        $(description: $description:tt,)?
        response: $response:tt $(,)?
    }) => {
        #[utoipa::path(
            get,
            path = $path,
            operation_id = $id,
            tag = $tag,
            summary = $summary,
            $(description = $description,)?
            params($params),
            responses((status = OK, description = $response, body = Vec<MemberItem>), WebError)
        )]
        pub async fn list_members(
            scope: <$owner as Owner>::ListMembersScope,
        ) -> WebResult<Json<BaseResponse<Vec<MemberItem>>>> {
            members::list::<$owner>(scope).await
        }
    };
    (add_member, $owner:tt, $params:tt, $role_owner:tt, $tag:tt, {
        path: $path:tt, id: $id:tt, summary: $summary:tt,
        $(description: $description:tt,)?
        response: $response:tt $(,)?
    }) => {
        #[utoipa::path(
            post,
            path = $path,
            operation_id = $id,
            tag = $tag,
            summary = $summary,
            $(description = $description,)?
            params($params),
            request_body = AddMemberRequest,
            responses((status = OK, description = $response, body = String, content_type = "application/json"), WebError)
        )]
        pub async fn add_member(
            scope: <$owner as Owner>::ManageMembersScope,
            state: State<Arc<ServerState>>,
            audit: Audit,
            Json(body): Json<AddMemberRequest>,
        ) -> WebResult<Json<BaseResponse<String>>> {
            members::add::<$owner>(scope, state, audit, Json(body)).await
        }
    };
    (change_role, $owner:tt, $params:tt, $role_owner:tt, $tag:tt, {
        path: $path:tt, id: $id:tt, summary: $summary:tt,
        $(description: $description:tt,)?
        response: $response:tt $(,)?
    }) => {
        #[utoipa::path(
            patch,
            path = $path,
            operation_id = $id,
            tag = $tag,
            summary = $summary,
            $(description = $description,)?
            params($params),
            request_body = AddMemberRequest,
            responses((status = OK, description = $response, body = String, content_type = "application/json"), WebError)
        )]
        pub async fn change_role(
            scope: <$owner as Owner>::ManageMembersScope,
            audit: Audit,
            Json(body): Json<AddMemberRequest>,
        ) -> WebResult<Json<BaseResponse<String>>> {
            members::change_role::<$owner>(scope, audit, Json(body)).await
        }
    };
    (remove_member, $owner:tt, $params:tt, $role_owner:tt, $tag:tt, {
        path: $path:tt, id: $id:tt, summary: $summary:tt,
        $(description: $description:tt,)?
        response: $response:tt $(,)?
    }) => {
        #[utoipa::path(
            delete,
            path = $path,
            operation_id = $id,
            tag = $tag,
            summary = $summary,
            $(description = $description,)?
            params($params),
            request_body = RemoveMemberRequest,
            responses((status = OK, description = $response, body = String, content_type = "application/json"), WebError)
        )]
        pub async fn remove_member(
            scope: <$owner as Owner>::ManageMembersScope,
            audit: Audit,
            Json(body): Json<RemoveMemberRequest>,
        ) -> WebResult<Json<BaseResponse<String>>> {
            members::remove::<$owner>(scope, audit, Json(body)).await
        }
    };
    (list_invitations, $owner:tt, $params:tt, $role_owner:tt, $tag:tt, {
        path: $path:tt, id: $id:tt, summary: $summary:tt,
        $(description: $description:tt,)?
        response: $response:tt $(,)?
    }) => {
        #[utoipa::path(
            get,
            path = $path,
            operation_id = $id,
            tag = $tag,
            summary = $summary,
            $(description = $description,)?
            params($params),
            responses((status = OK, description = $response, body = Vec<PendingInvitationItem>), WebError)
        )]
        pub async fn list_invitations(
            scope: <$owner as Owner>::ListInvitationsScope,
        ) -> WebResult<Json<BaseResponse<Vec<PendingInvitationItem>>>> {
            invitations::list::<$owner>(scope).await
        }
    };
    (revoke_invitation, $owner:tt, $params:tt, $role_owner:tt, $tag:tt, {
        path: $path:tt, id: $id:tt, summary: $summary:tt,
        $(description: $description:tt,)?
        response: $response:tt $(,)?
    }) => {
        #[utoipa::path(
            delete,
            path = $path,
            operation_id = $id,
            tag = $tag,
            summary = $summary,
            $(description = $description,)?
            params($params),
            request_body = RevokeInvitationRequest,
            responses((status = OK, description = $response, body = String, content_type = "application/json"), WebError)
        )]
        pub async fn revoke_invitation(
            scope: <$owner as Owner>::ManageMembersScope,
            audit: Audit,
            Json(body): Json<RevokeInvitationRequest>,
        ) -> WebResult<Json<BaseResponse<String>>> {
            invitations::revoke::<$owner>(scope, audit, Json(body)).await
        }
    };
    (list_roles, $owner:tt, $params:tt, $role_owner:tt, $tag:tt, {
        path: $path:tt, id: $id:tt, summary: $summary:tt,
        $(description: $description:tt,)?
        response: $response:tt $(,)?
    }) => {
        #[utoipa::path(
            get,
            path = $path,
            operation_id = $id,
            tag = $tag,
            summary = $summary,
            $(description = $description,)?
            params($params),
            responses((status = OK, description = $response, body = RoleListResponse<$role_owner>), WebError)
        )]
        pub async fn list_roles(
            scope: <$owner as Owner>::ReadRolesScope,
        ) -> WebResult<Json<BaseResponse<RoleListResponse<$role_owner>>>> {
            roles::list::<$owner>(scope).await
        }
    };
    (create_role, $owner:tt, $params:tt, $role_owner:tt, $tag:tt, {
        path: $path:tt, id: $id:tt, summary: $summary:tt,
        $(description: $description:tt,)?
        response: $response:tt $(,)?
    }) => {
        #[utoipa::path(
            post,
            path = $path,
            operation_id = $id,
            tag = $tag,
            summary = $summary,
            $(description = $description,)?
            params($params),
            request_body = CreateRoleRequest,
            responses((status = OK, description = $response, body = RoleResponse<$role_owner>), WebError)
        )]
        pub async fn create_role(
            scope: <$owner as Owner>::ManageRolesScope,
            audit: Audit,
            JsonBody(body): JsonBody<CreateRoleRequest>,
        ) -> WebResult<Json<BaseResponse<RoleResponse<$role_owner>>>> {
            roles::create::<$owner>(scope, audit, JsonBody(body)).await
        }
    };
    (show_role, $owner:tt, $params:tt, $role_owner:tt, $tag:tt, {
        path: $path:tt, id: $id:tt, summary: $summary:tt,
        $(description: $description:tt,)?
        response: $response:tt $(,)?
    }) => {
        #[utoipa::path(
            get,
            path = $path,
            operation_id = $id,
            tag = $tag,
            summary = $summary,
            $(description = $description,)?
            params($params, ("role_id" = RoleId, Path)),
            responses((status = OK, description = $response, body = RoleResponse<$role_owner>), WebError)
        )]
        pub async fn show_role(
            scope: <$owner as Owner>::ReadRolesScope,
        ) -> WebResult<Json<BaseResponse<RoleResponse<$role_owner>>>> {
            roles::show::<$owner>(scope).await
        }
    };
    (update_role, $owner:tt, $params:tt, $role_owner:tt, $tag:tt, {
        path: $path:tt, id: $id:tt, summary: $summary:tt,
        $(description: $description:tt,)?
        response: $response:tt $(,)?
    }) => {
        #[utoipa::path(
            patch,
            path = $path,
            operation_id = $id,
            tag = $tag,
            summary = $summary,
            $(description = $description,)?
            params($params, ("role_id" = RoleId, Path)),
            request_body = PatchRoleRequest,
            responses((status = OK, description = $response, body = RoleResponse<$role_owner>), WebError)
        )]
        pub async fn update_role(
            scope: <$owner as Owner>::ManageRolesScope,
            audit: Audit,
            JsonBody(body): JsonBody<PatchRoleRequest>,
        ) -> WebResult<Json<BaseResponse<RoleResponse<$role_owner>>>> {
            roles::update::<$owner>(scope, audit, JsonBody(body)).await
        }
    };
    (delete_role, $owner:tt, $params:tt, $role_owner:tt, $tag:tt, {
        path: $path:tt, id: $id:tt, summary: $summary:tt,
        $(description: $description:tt,)?
        response: $response:tt $(,)?
    }) => {
        #[utoipa::path(
            delete,
            path = $path,
            operation_id = $id,
            tag = $tag,
            summary = $summary,
            $(description = $description,)?
            params($params, ("role_id" = RoleId, Path)),
            responses((status = OK, description = $response, body = bool, content_type = "application/json"), WebError)
        )]
        pub async fn delete_role(
            scope: <$owner as Owner>::ManageRolesScope,
            audit: Audit,
        ) -> WebResult<Json<BaseResponse<bool>>> {
            roles::delete::<$owner>(scope, audit).await
        }
    };
}

pub mod project {
    use crate::api_spec::ProjectPath;
    use crate::endpoints::membership::project::{ProjectOwner, ProjectRoleOwner};

    owner_endpoints! {
        owner: ProjectOwner,
        params: ProjectPath,
        role_owner: ProjectRoleOwner,
        tag: "projects",
        list_members: {
            path: "/projects/{project}/users",
            id: "listProjectUsers",
            summary: "List project members",
            response: "Member list",
        },
        add_member: {
            path: "/projects/{project}/users",
            id: "addProjectUser",
            summary: "Invite member",
            description: "Invites a user to the project by username. Requires the caller to\nhold the **admin** role in the project.\n\nThe user is **not** a member until they accept. The response message is\n`Invitation sent`, and the pending row appears under\n`GET /projects/{project}/invitations` until it is accepted, declined or\nrevoked. An invitation expires after 7 days.\n\nA **superuser** who already holds the permission adds the member\ndirectly instead, answering `User added`. State-managed projects reject\nthis endpoint entirely; their members come from the state file.",
            response: "Invitation sent, or member added when the caller is a superuser",
        },
        change_role: {
            path: "/projects/{project}/users",
            id: "patchProjectUser",
            summary: "Update member role",
            description: "Updates the role or permissions of an existing project member.\nRequires the caller to hold the **admin** role in the project.",
            response: "Updated",
        },
        remove_member: {
            path: "/projects/{project}/users",
            id: "removeProjectUser",
            summary: "Remove member",
            description: "Removes a user from the project. Requires the caller to hold the\n**admin** role in the project.",
            response: "Member removed",
        },
        list_invitations: {
            path: "/projects/{project}/invitations",
            id: "listProjectInvitations",
            summary: "List pending invitations",
            description: "Invitations that have been sent but not yet accepted or declined.\nRequires `manageMembers` on the project.",
            response: "Pending invitations",
        },
        revoke_invitation: {
            path: "/projects/{project}/invitations",
            id: "revokeProjectInvitation",
            summary: "Revoke a pending invitation",
            description: "Requires `manageMembers` on the project.",
            response: "Invitation revoked",
        },
        list_roles: {
            path: "/projects/{project}/roles",
            id: "listProjectRoles",
            summary: "List roles available in the project",
            description: "Returns the three immutable built-in roles (Admin/Write/View) plus any\ncustom roles owned by this project, along with the catalogue of\npermissions that custom roles may carry.\n\nVisible to any member of the project so the add-member UI can\npopulate its role dropdown.",
            response: "Role catalogue",
        },
        create_role: {
            path: "/projects/{project}/roles",
            id: "createProjectRole",
            summary: "Create a custom role",
            description: "Creates a new role scoped to this project. The caller must hold\nthe **manageRoles** permission. The role's permission set is freely\nchosen from the capabilities returned by `GET /projects/{project}/roles` ->\n`available_permissions`. Built-in role names (Admin, Write, View) and\nany name already used by another role in the same project are rejected\nwith `409`.",
            response: "Role created",
        },
        show_role: {
            path: "/projects/{project}/roles/{role_id}",
            id: "getProjectRole",
            summary: "Fetch a role",
            response: "Role",
        },
        update_role: {
            path: "/projects/{project}/roles/{role_id}",
            id: "patchProjectRole",
            summary: "Update a custom role",
            description: "Renames a custom role and/or replaces its permission set. The three\nbuilt-in roles (Admin, Write, View) are immutable; attempts to modify\nthem return `403`.",
            response: "Role updated",
        },
        delete_role: {
            path: "/projects/{project}/roles/{role_id}",
            id: "deleteProjectRole",
            summary: "Delete a custom role",
            description: "Deletes a custom role. The built-in roles cannot be deleted (`403`).\nA role currently assigned to one or more members cannot be deleted\nuntil those members are reassigned (`400`).",
            response: "Role deleted",
        },
    }
}

pub mod cache {
    use crate::api_spec::CachePath;
    use crate::endpoints::membership::cache::{CacheOwner, CacheRoleOwner};

    owner_endpoints! {
        owner: CacheOwner,
        params: CachePath,
        role_owner: CacheRoleOwner,
        tag: "caches",
        list_members: {
            path: "/caches/{cache}/members",
            id: "listCacheMembers",
            summary: "List cache members",
            description: "Returns all members of the cache and their assigned roles. Requires `viewCache`.",
            response: "Member list",
        },
        add_member: {
            path: "/caches/{cache}/members",
            id: "addCacheMember",
            summary: "Invite cache member",
            description: "Invites a user to the cache with the given role. Requires\n`manageCacheMembers`.\n\nThe user is **not** a member until they accept; the response message is\n`Invitation sent` and the pending row appears under\n`GET /caches/{cache}/invitations`. An invitation expires after 7 days. A\n**superuser** who already holds the permission adds the member directly\ninstead, answering `User added`.",
            response: "Invitation sent, or member added when the caller is a superuser",
        },
        change_role: {
            path: "/caches/{cache}/members",
            id: "patchCacheMember",
            summary: "Update cache member role",
            description: "Changes the role of an existing cache member. Requires `manageCacheMembers`.",
            response: "Role updated",
        },
        remove_member: {
            path: "/caches/{cache}/members",
            id: "removeCacheMember",
            summary: "Remove cache member",
            description: "Removes a user from the cache. Requires `manageCacheMembers`. The last\nAdmin cannot be removed (`409`).",
            response: "Member removed",
        },
        list_invitations: {
            path: "/caches/{cache}/invitations",
            id: "listCacheInvitations",
            summary: "List pending cache invitations",
            description: "Requires `manageCacheMembers`.",
            response: "Pending invitations",
        },
        revoke_invitation: {
            path: "/caches/{cache}/invitations",
            id: "revokeCacheInvitation",
            summary: "Revoke a pending cache invitation",
            description: "Requires `manageCacheMembers`.",
            response: "Invitation revoked",
        },
        list_roles: {
            path: "/caches/{cache}/roles",
            id: "listCacheRoles",
            summary: "List cache roles",
            description: "Returns the three built-in roles (Admin/Write/View) plus any custom roles\nowned by this cache, along with the full catalogue of cache permissions.\nVisible to any cache member (`viewCache`).",
            response: "Role catalogue",
        },
        create_role: {
            path: "/caches/{cache}/roles",
            id: "createCacheRole",
            summary: "Create custom cache role",
            description: "Creates a new role scoped to this cache. Requires `manageCacheRoles`.\nThe role's permission set is chosen from the capabilities returned by\n`GET /caches/{cache}/roles` -> `available_permissions`.",
            response: "Role created",
        },
        show_role: {
            path: "/caches/{cache}/roles/{role_id}",
            id: "getCacheRole",
            summary: "Fetch a cache role",
            response: "Role",
        },
        update_role: {
            path: "/caches/{cache}/roles/{role_id}",
            id: "patchCacheRole",
            summary: "Update a custom cache role",
            description: "Renames a custom role and/or replaces its permission set. Built-in roles\n(Admin/Write/View) are immutable (`403`). Requires `manageCacheRoles`.",
            response: "Role updated",
        },
        delete_role: {
            path: "/caches/{cache}/roles/{role_id}",
            id: "deleteCacheRole",
            summary: "Delete a custom cache role",
            description: "Deletes a custom cache role. Built-in roles cannot be deleted (`403`).\nA role still assigned to members cannot be deleted until those members\nare reassigned (`400`). Requires `manageCacheRoles`.",
            response: "Role deleted",
        },
    }
}
