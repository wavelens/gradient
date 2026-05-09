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

use crate::types::consts::{BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID, BASE_ROLE_WRITE_ID};
use crate::types::ids::RoleId;

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
    /// CRUD on org webhooks.
    ManageWebhooks,
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
        Permission::ManageWebhooks,
        Permission::ManageWorkers,
        Permission::ManageSubscriptions,
        Permission::ManageSshKey,
        Permission::CreateProject,
        Permission::EditProject,
        Permission::TriggerEvaluation,
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
            Permission::ManageWebhooks => 6,
            Permission::ManageWorkers => 7,
            Permission::ManageSubscriptions => 8,
            Permission::ManageSshKey => 9,
            Permission::CreateProject => 10,
            Permission::EditProject => 11,
            Permission::TriggerEvaluation => 12,
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
            Permission::ManageWebhooks => "manageWebhooks",
            Permission::ManageWorkers => "manageWorkers",
            Permission::ManageSubscriptions => "manageSubscriptions",
            Permission::ManageSshKey => "manageSshKey",
            Permission::CreateProject => "createProject",
            Permission::EditProject => "editProject",
            Permission::TriggerEvaluation => "triggerEvaluation",
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

/// Canonical bitmask for the built-in **Write** role: project, webhook, and
/// integration management — but no member/role administration and no destruction
/// of the organization or its settings.
pub fn write_mask() -> PermissionMask {
    use Permission::*;
    mask_from(&[
        ViewOrg,
        ManageIntegrations,
        ManageWebhooks,
        ManageWorkers,
        ManageSubscriptions,
        ManageSshKey,
        CreateProject,
        EditProject,
        TriggerEvaluation,
    ])
}

/// Canonical bitmask for the built-in **View** role.
///
/// Read-only on sensitive surfaces (members, projects, webhooks, the org
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
        assert!(mask_grants(mask, Permission::ManageWebhooks));
    }

    #[test]
    fn view_mask_cannot_edit_projects_or_webhooks() {
        let mask = view_mask();
        assert!(!mask_grants(mask, Permission::EditProject));
        assert!(!mask_grants(mask, Permission::ManageWebhooks));
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
}
