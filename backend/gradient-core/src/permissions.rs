/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Permission system for org-scoped operations.
//!
//! Endpoint authorization is expressed in terms of [`Permission`] capabilities,
//! never in terms of role IDs. Each capability owns a stable bit position; a
//! role's set of granted capabilities is stored on `role.permission` as a
//! signed 64-bit bitmask, giving us 63 usable bits.
//!
//! The mapping between roles and capabilities therefore lives entirely in the
//! database. The three built-in roles (Admin/Write/View) are seeded with
//! canonical bitmasks at startup; organizations can additionally create their
//! own custom roles via the role-management API.

use gradient_types::consts::{BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID, BASE_ROLE_WRITE_ID};
use gradient_types::ids::RoleId;

/// A capability that a role may grant within an organization.
///
/// Permissions are intentionally granular so that custom roles can mix and
/// match (e.g. a "Releaser" role could hold [`Permission::TriggerEvaluation`]
/// without [`Permission::EditProject`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Permission {
    // ── Org-level ────────────────────────────────────────────────────────────
    /// View members-only content of a private organization.
    ViewOrg,
    /// Modify org settings (display name, description, etc.) and toggle the
    /// `public` flag.
    ManageOrgSettings,
    /// Delete the organization.
    DeleteOrg,
    /// Add, remove, or change roles for members.
    ManageMembers,
    /// Create, edit, or delete custom roles in the organization.
    ManageRoles,
    /// CRUD on org integrations (forge credentials).
    ManageIntegrations,
    /// CRUD on project actions.
    ManageActions,
    /// Register/configure org-owned workers.
    ManageWorkers,
    /// Subscribe / unsubscribe the org from caches.
    ManageSubscriptions,
    /// Manage the org's SSH key (used for git fetches).
    ManageSshKey,

    // ── Project-level (within an org) ────────────────────────────────────────
    /// Create a new project in the org.
    CreateProject,
    /// Modify or delete an existing project (settings, integration, transfer).
    EditProject,
    /// Trigger an evaluation / build run.
    TriggerEvaluation,
    /// CRUD on project triggers.
    ManageTriggers,
}

/// A bitmask over [`Permission`] capabilities. Stored on `role.permission`.
pub type PermissionMask = i64;

impl Permission {
    /// All permissions in canonical order. Used by the role-management API
    /// to emit the full list of capabilities a custom role may carry.
    pub const ALL: &'static [Permission] = &[
        Permission::ViewOrg,
        Permission::ManageOrgSettings,
        Permission::DeleteOrg,
        Permission::ManageMembers,
        Permission::ManageRoles,
        Permission::ManageIntegrations,
        Permission::ManageActions,
        Permission::ManageWorkers,
        Permission::ManageSubscriptions,
        Permission::ManageSshKey,
        Permission::CreateProject,
        Permission::EditProject,
        Permission::TriggerEvaluation,
        Permission::ManageTriggers,
    ];

    /// Stable bit position in the `role.permission` bitmask.
    ///
    /// **Wire format invariant:** never renumber an existing permission, only
    /// append new ones. Persisted role bitmasks depend on these positions.
    pub const fn bit(self) -> PermissionMask {
        let pos: u32 = match self {
            Permission::ViewOrg => 0,
            Permission::ManageOrgSettings => 1,
            Permission::DeleteOrg => 2,
            Permission::ManageMembers => 3,
            Permission::ManageRoles => 4,
            Permission::ManageIntegrations => 5,
            Permission::ManageActions => 6,
            Permission::ManageWorkers => 7,
            Permission::ManageSubscriptions => 8,
            Permission::ManageSshKey => 9,
            Permission::CreateProject => 10,
            Permission::EditProject => 11,
            Permission::TriggerEvaluation => 12,
            Permission::ManageTriggers => 13,
        };
        1_i64 << pos
    }

    /// Stable wire identifier (camelCase) used in the role-management API.
    pub const fn as_wire_name(self) -> &'static str {
        match self {
            Permission::ViewOrg => "viewOrg",
            Permission::ManageOrgSettings => "manageOrgSettings",
            Permission::DeleteOrg => "deleteOrg",
            Permission::ManageMembers => "manageMembers",
            Permission::ManageRoles => "manageRoles",
            Permission::ManageIntegrations => "manageIntegrations",
            Permission::ManageActions => "manageActions",
            Permission::ManageWorkers => "manageWorkers",
            Permission::ManageSubscriptions => "manageSubscriptions",
            Permission::ManageSshKey => "manageSshKey",
            Permission::CreateProject => "createProject",
            Permission::EditProject => "editProject",
            Permission::TriggerEvaluation => "triggerEvaluation",
            Permission::ManageTriggers => "manageTriggers",
        }
    }

    /// Parse a wire identifier back into a [`Permission`].
    pub fn from_wire_name(s: &str) -> Option<Self> {
        Permission::ALL
            .iter()
            .copied()
            .find(|p| p.as_wire_name() == s)
    }
}

/// True when `mask` grants `permission`.
#[inline]
pub const fn mask_grants(mask: PermissionMask, permission: Permission) -> bool {
    mask & permission.bit() != 0
}

/// Compose a bitmask from a slice of [`Permission`] values.
pub fn mask_from(perms: &[Permission]) -> PermissionMask {
    perms.iter().fold(0_i64, |acc, p| acc | p.bit())
}

/// Decompose a bitmask back into a `Vec<Permission>` in canonical order.
pub fn mask_to_vec(mask: PermissionMask) -> Vec<Permission> {
    Permission::ALL
        .iter()
        .copied()
        .filter(|p| mask_grants(mask, *p))
        .collect()
}

/// True when the permission represents a mutation (i.e. anything other than
/// pure viewing). Mutating permissions imply a state-managed-resource check.
pub fn is_mutating(permission: Permission) -> bool {
    !matches!(permission, Permission::ViewOrg)
}

// ── Built-in role bitmasks ───────────────────────────────────────────────────

/// Canonical bitmask for the built-in **Admin** role: every capability.
pub fn admin_mask() -> PermissionMask {
    mask_from(Permission::ALL)
}

/// Canonical bitmask for the built-in **Write** role: project, action, and
/// integration management - but no member/role administration and no destruction
/// of the organization or its settings.
pub fn write_mask() -> PermissionMask {
    use Permission::*;
    mask_from(&[
        ViewOrg,
        ManageIntegrations,
        ManageActions,
        ManageWorkers,
        ManageSubscriptions,
        ManageSshKey,
        CreateProject,
        EditProject,
        TriggerEvaluation,
        ManageTriggers,
    ])
}

/// Canonical bitmask for the built-in **View** role.
///
/// Read-only on sensitive surfaces (members, projects, actions, the org
/// itself), but currently retains mutation rights on a handful of non-secret
/// sub-resources (workers, ssh key, cache subscriptions, integrations) to
/// preserve historical behavior. Tightening these is an explicit follow-up.
pub fn view_mask() -> PermissionMask {
    use Permission::*;
    mask_from(&[
        ViewOrg,
        ManageIntegrations,
        ManageWorkers,
        ManageSubscriptions,
        ManageSshKey,
    ])
}

// ── Built-in role identification ─────────────────────────────────────────────

/// True if `role_id` is one of the immutable built-in roles. Built-in roles
/// cannot be edited or deleted via the role-management API.
pub fn is_builtin_role(role_id: RoleId) -> bool {
    role_id == BASE_ROLE_ADMIN_ID || role_id == BASE_ROLE_WRITE_ID || role_id == BASE_ROLE_VIEW_ID
}

// ── CachePermission ──────────────────────────────────────────────────────────

use gradient_types::consts::{
    BASE_CACHE_ROLE_ADMIN_ID, BASE_CACHE_ROLE_VIEW_ID, BASE_CACHE_ROLE_WRITE_ID,
};

/// A capability granted by a cache-scoped role. Stored in `cache_role.permission`
/// as a 64-bit bitmask, parallel to (but disjoint from) [`Permission`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum CachePermission {
    ViewCache,
    ReadStore,
    WriteStore,
    ManageCacheSettings,
    ManageCacheKeys,
    ManageCacheUpstreams,
    ManageCacheMembers,
    ManageCacheRoles,
    ManageCacheSubscriptions,
    DeleteCache,
}

impl CachePermission {
    pub const ALL: &'static [CachePermission] = &[
        CachePermission::ViewCache,
        CachePermission::ReadStore,
        CachePermission::WriteStore,
        CachePermission::ManageCacheSettings,
        CachePermission::ManageCacheKeys,
        CachePermission::ManageCacheUpstreams,
        CachePermission::ManageCacheMembers,
        CachePermission::ManageCacheRoles,
        CachePermission::ManageCacheSubscriptions,
        CachePermission::DeleteCache,
    ];

    pub const fn bit(self) -> PermissionMask {
        let pos: u32 = match self {
            CachePermission::ViewCache => 0,
            CachePermission::ReadStore => 1,
            CachePermission::WriteStore => 2,
            CachePermission::ManageCacheSettings => 3,
            CachePermission::ManageCacheKeys => 4,
            CachePermission::ManageCacheUpstreams => 5,
            CachePermission::ManageCacheMembers => 6,
            CachePermission::ManageCacheRoles => 7,
            CachePermission::ManageCacheSubscriptions => 8,
            CachePermission::DeleteCache => 9,
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
            CachePermission::ManageCacheUpstreams => "manageCacheUpstreams",
            CachePermission::ManageCacheMembers => "manageCacheMembers",
            CachePermission::ManageCacheRoles => "manageCacheRoles",
            CachePermission::ManageCacheSubscriptions => "manageCacheSubscriptions",
            CachePermission::DeleteCache => "deleteCache",
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

pub fn is_builtin_cache_role(role_id: RoleId) -> bool {
    role_id == BASE_CACHE_ROLE_ADMIN_ID
        || role_id == BASE_CACHE_ROLE_WRITE_ID
        || role_id == BASE_CACHE_ROLE_VIEW_ID
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_permission_has_unique_bit() {
        let mut seen = 0_i64;
        for p in Permission::ALL.iter().copied() {
            assert_eq!(p.bit() & seen, 0, "{:?} re-uses an earlier bit", p);
            seen |= p.bit();
        }
    }

    #[test]
    fn wire_names_round_trip() {
        for p in Permission::ALL.iter().copied() {
            assert_eq!(Permission::from_wire_name(p.as_wire_name()), Some(p));
        }
        assert_eq!(Permission::from_wire_name("nope"), None);
    }

    #[test]
    fn admin_mask_grants_everything() {
        let mask = admin_mask();
        for p in Permission::ALL.iter().copied() {
            assert!(mask_grants(mask, p), "admin missing {:?}", p);
        }
    }

    #[test]
    fn write_mask_excludes_admin_only_perms() {
        let mask = write_mask();
        assert!(!mask_grants(mask, Permission::ManageMembers));
        assert!(!mask_grants(mask, Permission::ManageRoles));
        assert!(!mask_grants(mask, Permission::DeleteOrg));
        assert!(!mask_grants(mask, Permission::ManageOrgSettings));
        assert!(mask_grants(mask, Permission::EditProject));
        assert!(mask_grants(mask, Permission::ManageActions));
    }

    #[test]
    fn view_mask_cannot_edit_projects_or_actions() {
        let mask = view_mask();
        assert!(!mask_grants(mask, Permission::EditProject));
        assert!(!mask_grants(mask, Permission::ManageActions));
        assert!(!mask_grants(mask, Permission::ManageMembers));
        assert!(!mask_grants(mask, Permission::ManageRoles));
        assert!(mask_grants(mask, Permission::ViewOrg));
    }

    #[test]
    fn empty_mask_grants_nothing() {
        for p in Permission::ALL.iter().copied() {
            assert!(!mask_grants(0, p));
        }
    }

    #[test]
    fn mask_round_trips_through_vec() {
        let mask = write_mask();
        let perms = mask_to_vec(mask);
        assert_eq!(mask_from(&perms), mask);
    }

    #[test]
    fn view_org_is_not_mutating() {
        assert!(!is_mutating(Permission::ViewOrg));
        assert!(is_mutating(Permission::EditProject));
        assert!(is_mutating(Permission::ManageMembers));
        assert!(is_mutating(Permission::ManageRoles));
    }

    #[test]
    fn is_builtin_role_recognises_seed_uuids() {
        assert!(is_builtin_role(BASE_ROLE_ADMIN_ID));
        assert!(is_builtin_role(BASE_ROLE_WRITE_ID));
        assert!(is_builtin_role(BASE_ROLE_VIEW_ID));
        let other = RoleId::new(uuid::uuid!("99999999-9999-9999-9999-999999999999"));
        assert!(!is_builtin_role(other));
    }

    // ── CachePermission tests ────────────────────────────────────────────────

    #[test]
    fn each_cache_permission_has_unique_bit() {
        let mut seen = 0_i64;
        for p in CachePermission::ALL.iter().copied() {
            assert_eq!(p.bit() & seen, 0, "{:?} re-uses an earlier bit", p);
            seen |= p.bit();
        }
    }

    #[test]
    fn cache_wire_names_round_trip() {
        for p in CachePermission::ALL.iter().copied() {
            assert_eq!(CachePermission::from_wire_name(p.as_wire_name()), Some(p));
        }
        assert_eq!(CachePermission::from_wire_name("nope"), None);
    }

    #[test]
    fn cache_admin_mask_grants_everything() {
        let mask = cache_admin_mask();
        for p in CachePermission::ALL.iter().copied() {
            assert!(cache_mask_grants(mask, p), "admin missing {:?}", p);
        }
    }

    #[test]
    fn cache_write_mask_excludes_admin_only() {
        let mask = cache_write_mask();
        assert!(!cache_mask_grants(
            mask,
            CachePermission::ManageCacheSettings
        ));
        assert!(!cache_mask_grants(mask, CachePermission::ManageCacheRoles));
        assert!(!cache_mask_grants(mask, CachePermission::DeleteCache));
        assert!(cache_mask_grants(mask, CachePermission::WriteStore));
        assert!(cache_mask_grants(mask, CachePermission::ReadStore));
        assert!(cache_mask_grants(mask, CachePermission::ViewCache));
    }

    #[test]
    fn cache_view_mask_is_read_only() {
        let mask = cache_view_mask();
        assert!(cache_mask_grants(mask, CachePermission::ViewCache));
        assert!(cache_mask_grants(mask, CachePermission::ReadStore));
        assert!(!cache_mask_grants(mask, CachePermission::WriteStore));
        assert!(!cache_mask_grants(mask, CachePermission::ManageCacheKeys));
    }

    #[test]
    fn cache_view_is_not_mutating() {
        assert!(!is_cache_mutating(CachePermission::ViewCache));
        assert!(!is_cache_mutating(CachePermission::ReadStore));
        assert!(is_cache_mutating(CachePermission::WriteStore));
        assert!(is_cache_mutating(CachePermission::ManageCacheKeys));
        assert!(is_cache_mutating(CachePermission::DeleteCache));
    }

    #[test]
    fn cache_mask_round_trips_through_vec() {
        let mask = cache_write_mask();
        let perms = cache_mask_to_vec(mask);
        assert_eq!(cache_mask_from(&perms), mask);
    }

    #[test]
    fn is_builtin_cache_role_recognises_seed_uuids() {
        use gradient_types::consts::{
            BASE_CACHE_ROLE_ADMIN_ID, BASE_CACHE_ROLE_VIEW_ID, BASE_CACHE_ROLE_WRITE_ID,
        };
        assert!(is_builtin_cache_role(BASE_CACHE_ROLE_ADMIN_ID));
        assert!(is_builtin_cache_role(BASE_CACHE_ROLE_WRITE_ID));
        assert!(is_builtin_cache_role(BASE_CACHE_ROLE_VIEW_ID));
        let other = RoleId::new(uuid::uuid!("99999999-9999-9999-9999-999999999999"));
        assert!(!is_builtin_cache_role(other));
    }
}
