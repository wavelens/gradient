/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::consts::{BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID, BASE_ROLE_WRITE_ID};
use crate::ids::RoleId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum Permission {
    ViewProject,
    ManageProjectSettings,
    DeleteProject,
    ManageMembers,
    ManageRoles,
    ManageIntegrations,
    ManageActions,
    ManageWorkers,
    ManageSubscriptions,
    ManageSshKey,

    CreateTask,
    EditTask,
    TriggerEvaluation,
    ManageTriggers,
    ManageWebhooks,

    CreateProject,
}

pub type PermissionMask = i64;

impl Permission {
    pub const ALL: &'static [Permission] = &[
        Permission::ViewProject,
        Permission::ManageProjectSettings,
        Permission::DeleteProject,
        Permission::ManageMembers,
        Permission::ManageRoles,
        Permission::ManageIntegrations,
        Permission::ManageActions,
        Permission::ManageWorkers,
        Permission::ManageSubscriptions,
        Permission::ManageSshKey,
        Permission::CreateTask,
        Permission::EditTask,
        Permission::TriggerEvaluation,
        Permission::ManageTriggers,
        Permission::ManageWebhooks,
        Permission::CreateProject,
    ];

    /// New permissions must be appended, and an existing one must never be renumbered.
    /// Persisted role bitmasks are depending on these positions.
    pub const fn bit(self) -> PermissionMask {
        let pos: u32 = match self {
            Permission::ViewProject => 0,
            Permission::ManageProjectSettings => 1,
            Permission::DeleteProject => 2,
            Permission::ManageMembers => 3,
            Permission::ManageRoles => 4,
            Permission::ManageIntegrations => 5,
            Permission::ManageActions => 6,
            Permission::ManageWorkers => 7,
            Permission::ManageSubscriptions => 8,
            Permission::ManageSshKey => 9,
            Permission::CreateTask => 10,
            Permission::EditTask => 11,
            Permission::TriggerEvaluation => 12,
            Permission::ManageTriggers => 13,
            Permission::ManageWebhooks => 14,
            Permission::CreateProject => 15,
        };
        1_i64 << pos
    }

    pub const fn as_wire_name(self) -> &'static str {
        match self {
            Permission::ViewProject => "viewProject",
            Permission::ManageProjectSettings => "manageProjectSettings",
            Permission::DeleteProject => "deleteProject",
            Permission::ManageMembers => "manageMembers",
            Permission::ManageRoles => "manageRoles",
            Permission::ManageIntegrations => "manageIntegrations",
            Permission::ManageActions => "manageActions",
            Permission::ManageWorkers => "manageWorkers",
            Permission::ManageSubscriptions => "manageSubscriptions",
            Permission::ManageSshKey => "manageSshKey",
            Permission::CreateTask => "createTask",
            Permission::EditTask => "editTask",
            Permission::TriggerEvaluation => "triggerEvaluation",
            Permission::ManageTriggers => "manageTriggers",
            Permission::ManageWebhooks => "manageWebhooks",
            Permission::CreateProject => "createProject",
        }
    }

    pub const fn is_server_wide(self) -> bool {
        matches!(self, Permission::CreateProject)
    }

    pub const fn permission_explanation(self) -> &'static str {
        match self {
            Permission::ViewProject => "View project, its tasks, evaluations and build logs.",
            Permission::ManageProjectSettings => "Change project name, description and settings.",
            Permission::DeleteProject => "Delete the project and all tasks and builds.",
            Permission::ManageMembers => "Invite and remove members and change roles.",
            Permission::ManageRoles => "Create, edit and delete custom roles.",
            Permission::ManageIntegrations => {
                "Add or edit a project's connection to a Git host and install webhooks and deploy keys."
            }
            Permission::ManageActions => "Set actions to run on task events.",
            Permission::ManageWorkers => "Register, enable and disable project workers.",
            Permission::ManageSubscriptions => {
                "Subscribe a project to the cache, change the mode and output signing, or remove the subscription."
            }
            Permission::ManageSshKey => "Update the project SSH key for repository clones.",
            Permission::CreateTask => "Add a new task to the project.",
            Permission::EditTask => "Change the task repository, branch and settings.",
            Permission::TriggerEvaluation => "Start an evaluation manually.",
            Permission::ManageTriggers => "Pick when tasks run (e.g., on push, on schedule)",
            Permission::ManageWebhooks => "Send event notifications to an external URL.",
            Permission::CreateProject => "Create a new project on the server.",
        }
    }

    pub const fn permission_label(self) -> &'static str {
        match self {
            Permission::ViewProject => "view project",
            Permission::ManageProjectSettings => "project settings",
            Permission::DeleteProject => "delete project",
            Permission::ManageMembers => "members",
            Permission::ManageRoles => "roles",
            Permission::ManageIntegrations => "Connection to Git Host",
            Permission::ManageActions => "actions",
            Permission::ManageWorkers => "workers",
            Permission::ManageSubscriptions => "subscriptions",
            Permission::ManageSshKey => "ssh keys",
            Permission::CreateTask => "create task",
            Permission::EditTask => "edit task",
            Permission::TriggerEvaluation => "trigger evaluation",
            Permission::ManageTriggers => "triggers",
            Permission::ManageWebhooks => "webhooks",
            Permission::CreateProject => "create project",
        }
    }

    pub fn from_wire_name(s: &str) -> Option<Self> {
        Permission::ALL
            .iter()
            .copied()
            .find(|p| p.as_wire_name() == s)
    }
}

#[inline]
pub const fn mask_grants(mask: PermissionMask, permission: Permission) -> bool {
    mask & permission.bit() != 0
}

pub fn mask_from(perms: &[Permission]) -> PermissionMask {
    perms.iter().fold(0_i64, |acc, p| acc | p.bit())
}

pub fn mask_to_vec(mask: PermissionMask) -> Vec<Permission> {
    Permission::ALL
        .iter()
        .copied()
        .filter(|p| mask_grants(mask, *p))
        .collect()
}

pub fn is_mutating(permission: Permission) -> bool {
    !matches!(permission, Permission::ViewProject)
}

pub fn server_wide_mask() -> PermissionMask {
    Permission::ALL
        .iter()
        .filter(|p| p.is_server_wide())
        .fold(0, |acc, p| acc | p.bit())
}

pub fn admin_mask() -> PermissionMask {
    mask_from(Permission::ALL) & !server_wide_mask()
}

pub fn write_mask() -> PermissionMask {
    use Permission::*;
    mask_from(&[
        ViewProject,
        ManageIntegrations,
        ManageActions,
        ManageWorkers,
        ManageSubscriptions,
        ManageSshKey,
        CreateTask,
        EditTask,
        TriggerEvaluation,
        ManageTriggers,
        ManageWebhooks,
    ])
}

pub fn view_mask() -> PermissionMask {
    Permission::ViewProject.bit()
}

pub const fn mask_with_view_project_permission(mask: PermissionMask) -> PermissionMask {
    mask | Permission::ViewProject.bit()
}

pub fn is_builtin_role(role_id: RoleId) -> bool {
    role_id == BASE_ROLE_ADMIN_ID || role_id == BASE_ROLE_WRITE_ID || role_id == BASE_ROLE_VIEW_ID
}

use crate::consts::{BASE_CACHE_ROLE_ADMIN_ID, BASE_CACHE_ROLE_VIEW_ID, BASE_CACHE_ROLE_WRITE_ID};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum CachePermission {
    ViewCache,
    ReadStore,
    WriteStore,
    ManageCacheSettings,
    ManageCacheKeys,
    ManageUpstreamCaches,
    ManageCacheMembers,
    ManageCacheRoles,
    ManageCacheSubscriptions,
    DeleteCache,
    ManageCacheWebhooks,
}

impl CachePermission {
    pub const ALL: &'static [CachePermission] = &[
        CachePermission::ViewCache,
        CachePermission::ReadStore,
        CachePermission::WriteStore,
        CachePermission::ManageCacheSettings,
        CachePermission::ManageCacheKeys,
        CachePermission::ManageUpstreamCaches,
        CachePermission::ManageCacheMembers,
        CachePermission::ManageCacheRoles,
        CachePermission::ManageCacheSubscriptions,
        CachePermission::DeleteCache,
        CachePermission::ManageCacheWebhooks,
    ];

    pub const fn bit(self) -> PermissionMask {
        let pos: u32 = match self {
            CachePermission::ViewCache => 0,
            CachePermission::ReadStore => 1,
            CachePermission::WriteStore => 2,
            CachePermission::ManageCacheSettings => 3,
            CachePermission::ManageCacheKeys => 4,
            CachePermission::ManageUpstreamCaches => 5,
            CachePermission::ManageCacheMembers => 6,
            CachePermission::ManageCacheRoles => 7,
            CachePermission::ManageCacheSubscriptions => 8,
            CachePermission::DeleteCache => 9,
            CachePermission::ManageCacheWebhooks => 10,
        };
        1_i64 << pos
    }

    pub const fn as_wire_name(self) -> &'static str {
        match self {
            CachePermission::ViewCache => "viewCache",
            CachePermission::ReadStore => "readStore",
            CachePermission::WriteStore => "writeStore",
            CachePermission::ManageCacheSettings => "manageCacheSettings",
            CachePermission::ManageCacheKeys => "manageCacheKeys",
            CachePermission::ManageUpstreamCaches => "manageUpstreamCaches",
            CachePermission::ManageCacheMembers => "manageCacheMembers",
            CachePermission::ManageCacheRoles => "manageCacheRoles",
            CachePermission::ManageCacheSubscriptions => "manageCacheSubscriptions",
            CachePermission::DeleteCache => "deleteCache",
            CachePermission::ManageCacheWebhooks => "manageCacheWebhooks",
        }
    }

    pub const fn permission_label(self) -> &'static str {
        match self {
            CachePermission::ViewCache => "view cache",
            CachePermission::ReadStore => "read cache",
            CachePermission::WriteStore => "write cache",
            CachePermission::ManageCacheSettings => "manage cache settings",
            CachePermission::ManageCacheKeys => "manage cache keys",
            CachePermission::ManageUpstreamCaches => "manage upstream caches",
            CachePermission::ManageCacheMembers => "manage cache members",
            CachePermission::ManageCacheRoles => "manage cache roles",
            CachePermission::ManageCacheSubscriptions => "manage cache subscriptions",
            CachePermission::DeleteCache => "delete cache",
            CachePermission::ManageCacheWebhooks => "manage event notifications",
        }
    }

    pub const fn permission_explanation(self) -> &'static str {
        match self {
            CachePermission::ViewCache => "Show the cache, its members and roles in Gradient.",
            CachePermission::ReadStore => "Download packages from the cache in Nix.",
            CachePermission::WriteStore => "Upload build outputs to the cache.",
            CachePermission::ManageCacheSettings => {
                "Change the cache name, visibility and settings."
            }
            CachePermission::ManageCacheKeys => "Read the secret signing key for cache packages",
            CachePermission::ManageUpstreamCaches => "Select other caches to fetch packages from.",
            CachePermission::ManageCacheMembers => "Invite or remove members and change roles.",
            CachePermission::ManageCacheRoles => "Create, edit and delete custom roles.",
            CachePermission::ManageCacheSubscriptions => {
                "Approve or deny projects that request use of the cache."
            }
            CachePermission::DeleteCache => "Delete the cache along with all packages.",
            CachePermission::ManageCacheWebhooks => "Send event notifications to an external URL.",
        }
    }

    pub fn from_wire_name(s: &str) -> Option<Self> {
        CachePermission::ALL
            .iter()
            .copied()
            .find(|p| p.as_wire_name() == s)
    }
}

#[inline]
pub const fn cache_mask_grants(mask: PermissionMask, permission: CachePermission) -> bool {
    mask & permission.bit() != 0
}

pub fn cache_mask_from(perms: &[CachePermission]) -> PermissionMask {
    perms.iter().fold(0_i64, |acc, p| acc | p.bit())
}

pub fn cache_mask_to_vec(mask: PermissionMask) -> Vec<CachePermission> {
    CachePermission::ALL
        .iter()
        .copied()
        .filter(|p| cache_mask_grants(mask, *p))
        .collect()
}

pub fn is_cache_mutating(permission: CachePermission) -> bool {
    !matches!(
        permission,
        CachePermission::ViewCache | CachePermission::ReadStore
    )
}

pub fn cache_admin_mask() -> PermissionMask {
    cache_mask_from(CachePermission::ALL)
}

pub fn cache_write_mask() -> PermissionMask {
    use CachePermission::*;
    cache_mask_from(&[ViewCache, ReadStore, WriteStore])
}

pub fn cache_view_mask() -> PermissionMask {
    use CachePermission::*;
    cache_mask_from(&[ViewCache, ReadStore])
}

pub const fn cache_mask_with_view_permission_set(mask: PermissionMask) -> PermissionMask {
    mask | CachePermission::ViewCache.bit()
}

pub fn is_builtin_cache_role(role_id: RoleId) -> bool {
    role_id == BASE_CACHE_ROLE_ADMIN_ID
        || role_id == BASE_CACHE_ROLE_WRITE_ID
        || role_id == BASE_CACHE_ROLE_VIEW_ID
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_bits_are_unique_and_wire_names_round_trip() {
        let mut seen = 0_i64;
        for p in Permission::ALL.iter().copied() {
            assert_eq!(p.bit() & seen, 0, "{p:?} re-uses an earlier bit");
            seen |= p.bit();
            assert_eq!(Permission::from_wire_name(p.as_wire_name()), Some(p));
        }
        assert_eq!(Permission::from_wire_name("nope"), None);

        let mut seen = 0_i64;
        for p in CachePermission::ALL.iter().copied() {
            assert_eq!(p.bit() & seen, 0, "{p:?} re-uses an earlier bit");
            seen |= p.bit();
            assert_eq!(CachePermission::from_wire_name(p.as_wire_name()), Some(p));
        }
        assert_eq!(CachePermission::from_wire_name("nope"), None);
    }

    #[test]
    fn serialized_names_match_wire_names() {
        for p in Permission::ALL.iter().copied() {
            assert_eq!(serde_json::to_value(p).unwrap(), p.as_wire_name());
        }
        for p in CachePermission::ALL.iter().copied() {
            assert_eq!(serde_json::to_value(p).unwrap(), p.as_wire_name());
        }
    }

    #[test]
    fn masks_grant_and_withhold_the_expected_permissions() {
        for p in Permission::ALL.iter().copied() {
            assert_eq!(
                mask_grants(admin_mask(), p),
                !p.is_server_wide(),
                "admin role and {p:?}"
            );
            assert!(!mask_grants(0, p), "empty mask granted {p:?}");
        }
        assert!(!mask_grants(admin_mask(), Permission::CreateProject));

        let write = write_mask();
        assert!(!mask_grants(write, Permission::ManageMembers));
        assert!(!mask_grants(write, Permission::ManageRoles));
        assert!(!mask_grants(write, Permission::DeleteProject));
        assert!(!mask_grants(write, Permission::ManageProjectSettings));
        assert!(mask_grants(write, Permission::EditTask));
        assert!(mask_grants(write, Permission::ManageActions));

        let view = view_mask();
        assert!(!mask_grants(view, Permission::EditTask));
        assert!(!mask_grants(view, Permission::ManageActions));
        assert!(!mask_grants(view, Permission::ManageMembers));
        assert!(!mask_grants(view, Permission::ManageRoles));
        assert!(mask_grants(view, Permission::ViewProject));

        for p in CachePermission::ALL.iter().copied() {
            assert!(
                cache_mask_grants(cache_admin_mask(), p),
                "admin missing {p:?}"
            );
        }

        let cache_write = cache_write_mask();
        assert!(!cache_mask_grants(
            cache_write,
            CachePermission::ManageCacheSettings
        ));
        assert!(!cache_mask_grants(
            cache_write,
            CachePermission::ManageCacheRoles
        ));
        assert!(!cache_mask_grants(
            cache_write,
            CachePermission::DeleteCache
        ));
        assert!(cache_mask_grants(cache_write, CachePermission::WriteStore));
        assert!(cache_mask_grants(cache_write, CachePermission::ReadStore));
        assert!(cache_mask_grants(cache_write, CachePermission::ViewCache));

        let cache_view = cache_view_mask();
        assert!(cache_mask_grants(cache_view, CachePermission::ViewCache));
        assert!(cache_mask_grants(cache_view, CachePermission::ReadStore));
        assert!(!cache_mask_grants(cache_view, CachePermission::WriteStore));
        assert!(!cache_mask_grants(
            cache_view,
            CachePermission::ManageCacheKeys
        ));
    }

    #[test]
    fn all_project_and_cache_permissions_have_short_non_empty_descriptions_within_length_limit() {
        let explanations = Permission::ALL
            .iter()
            .map(|p| (p.as_wire_name(), p.permission_explanation()))
            .chain(
                CachePermission::ALL
                    .iter()
                    .map(|p| (p.as_wire_name(), p.permission_explanation())),
            );
        for (id, explanation) in explanations {
            assert!(
                (1..=60).contains(&explanation.len()),
                "{id}: {explanation:?}"
            );
        }
    }

    #[test]
    fn masks_round_trip_through_vectors() {
        let write = write_mask();
        assert_eq!(mask_from(&mask_to_vec(write)), write);
        let cache_write = cache_write_mask();
        assert_eq!(
            cache_mask_from(&cache_mask_to_vec(cache_write)),
            cache_write
        );
    }

    #[test]
    fn mutating_permissions_cover_every_write_permission() {
        assert!(!is_mutating(Permission::ViewProject));
        assert!(is_mutating(Permission::EditTask));
        assert!(is_mutating(Permission::ManageMembers));
        assert!(is_mutating(Permission::ManageRoles));

        assert!(!is_cache_mutating(CachePermission::ViewCache));
        assert!(!is_cache_mutating(CachePermission::ReadStore));
        assert!(is_cache_mutating(CachePermission::WriteStore));
        assert!(is_cache_mutating(CachePermission::ManageCacheKeys));
        assert!(is_cache_mutating(CachePermission::DeleteCache));
    }
}
