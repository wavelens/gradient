/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default, utoipa::ToSchema)]
#[cfg_attr(
    feature = "server-only-sea-orm-and-trait-derives",
    derive(clap::ValueEnum)
)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(
    feature = "server-only-sea-orm-and-trait-derives",
    value(rename_all = "lowercase")
)]
pub enum CreatePermission {
    /// Nobody via the API. Only the declarative state import may create them.
    None,
    /// Superusers only.
    Superusers,
    /// Any authenticated user.
    #[default]
    Everyone,
}

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
    OpenJobTerminal,

    CreateProject,
    ReadSuperuser,
    WriteSuperuser,
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
        Permission::OpenJobTerminal,
        Permission::CreateProject,
        Permission::ReadSuperuser,
        Permission::WriteSuperuser,
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
            Permission::WriteSuperuser => 16,
            Permission::ReadSuperuser => 17,
            Permission::OpenJobTerminal => 18,
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
            Permission::OpenJobTerminal => "openJobTerminal",
            Permission::CreateProject => "createProject",
            Permission::ReadSuperuser => "readSuperuser",
            Permission::WriteSuperuser => "writeSuperuser",
        }
    }

    pub const fn is_server_wide(self) -> bool {
        matches!(
            self,
            Permission::CreateProject | Permission::ReadSuperuser | Permission::WriteSuperuser
        )
    }

    pub const fn permission_explanation(self) -> &'static str {
        match self {
            Permission::ViewProject => "View project, its tasks, evaluations and build logs.",
            Permission::ManageProjectSettings => "Change project name, description and settings.",
            Permission::DeleteProject => "Delete the project and all tasks and builds.",
            Permission::ManageMembers => "Invite and remove members and change roles.",
            Permission::ManageRoles => "Create, edit and delete custom roles.",
            Permission::ManageIntegrations => {
                "Manage connections for Git hosts, webhooks, and deploy keys."
            }
            Permission::ManageActions => "Set actions to run on task events.",
            Permission::ManageWorkers => "Register, enable and disable project workers.",
            Permission::ManageSubscriptions => {
                "Subscribe to a cache, set mode and signing, or unsubscribe."
            }
            Permission::ManageSshKey => "Update the project SSH key for repository clones.",
            Permission::CreateTask => "Add a new task to the project.",
            Permission::EditTask => "Change the task repository, branch and settings.",
            Permission::TriggerEvaluation => "Start an evaluation manually.",
            Permission::ManageTriggers => "Pick when tasks run (e.g., on push, on schedule)",
            Permission::ManageWebhooks => "Send event notifications to an external URL.",
            Permission::OpenJobTerminal => {
                "Open a shell, console, or port forward in a running job."
            }
            Permission::CreateProject => "Create a new project on the server.",
            Permission::ReadSuperuser => "Use owner's superuser rights on GET, HEAD, OPTIONS",
            Permission::WriteSuperuser => {
                "Use owner's superuser rights on POST, PUT, PATCH and DELETE"
            }
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
            Permission::OpenJobTerminal => "open job terminal",
            Permission::CreateProject => "create project",
            Permission::ReadSuperuser => "superuser read",
            Permission::WriteSuperuser => "superuser write",
        }
    }

    pub fn from_wire_name(s: &str) -> Option<Self> {
        Permission::ALL
            .iter()
            .copied()
            .find(|p| p.as_wire_name() == s)
    }
}

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

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct PermissionEntry {
    pub id: String,
    pub mutating: bool,
    pub permission_explanation: String,
}

impl From<Permission> for PermissionEntry {
    fn from(permission: Permission) -> Self {
        Self {
            id: permission.as_wire_name().to_string(),
            mutating: is_mutating(permission),
            permission_explanation: permission.permission_explanation().to_string(),
        }
    }
}

impl From<CachePermission> for PermissionEntry {
    fn from(permission: CachePermission) -> Self {
        Self {
            id: permission.as_wire_name().to_string(),
            mutating: is_cache_mutating(permission),
            permission_explanation: permission.permission_explanation().to_string(),
        }
    }
}

pub fn is_mutating(permission: Permission) -> bool {
    !matches!(permission, Permission::ViewProject)
}

pub fn is_cache_mutating(permission: CachePermission) -> bool {
    !matches!(
        permission,
        CachePermission::ViewCache | CachePermission::ReadStore
    )
}
