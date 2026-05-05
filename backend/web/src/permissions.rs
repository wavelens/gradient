/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Permission system for org-scoped operations.
//!
//! Endpoint authorization is expressed in terms of [`Permission`] capabilities,
//! never in terms of role IDs. This module owns the only mapping from a
//! membership row's `role` UUID to the set of permissions it grants.
//!
//! Today the mapping is hardcoded for the three built-in roles. When custom
//! roles ship, [`role_grants`] will be replaced with a DB-backed lookup
//! against a `role_permissions` table; nothing outside this module needs to
//! change.

use gradient_core::types::consts::{BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID, BASE_ROLE_WRITE_ID};
use uuid::Uuid;
use gradient_core::types::ids::*;

/// A capability that a role may grant within an organization.
///
/// Permissions are intentionally granular so that custom roles can mix and
/// match (e.g. a "Releaser" role could hold [`Permission::TriggerEvaluation`]
/// without [`Permission::EditProject`]). The mapping for built-in roles is
/// chosen to preserve historical behavior — see [`role_grants`].
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

/// Returns true when `role_id` (a value of `organization_user.role`) grants
/// `permission`. Unknown roles grant nothing.
///
/// **Custom-roles migration point.** When custom roles land, replace the body
/// of this function with a DB lookup against `role_permissions`; the call
/// sites need not change.
pub fn role_grants(role_id: RoleId, permission: Permission) -> bool {
    use Permission::*;

    let granted: &[Permission] = if role_id == BASE_ROLE_ADMIN_ID {
        &[
            ViewOrg,
            ManageOrgSettings,
            DeleteOrg,
            ManageMembers,
            ManageIntegrations,
            ManageWebhooks,
            ManageWorkers,
            ManageSubscriptions,
            ManageSshKey,
            CreateProject,
            EditProject,
            TriggerEvaluation,
        ]
    } else if role_id == BASE_ROLE_WRITE_ID {
        &[
            ViewOrg,
            ManageIntegrations,
            ManageWebhooks,
            ManageWorkers,
            ManageSubscriptions,
            ManageSshKey,
            CreateProject,
            EditProject,
            TriggerEvaluation,
        ]
    } else if role_id == BASE_ROLE_VIEW_ID {
        // View role is read-only for sensitive surfaces but currently retains
        // mutation rights on a few "non-secret" sub-resources (workers, ssh
        // key, cache subscriptions, integrations). This preserves historical
        // behavior; tightening these is an explicit follow-up.
        &[
            ViewOrg,
            ManageIntegrations,
            ManageWorkers,
            ManageSubscriptions,
            ManageSshKey,
        ]
    } else {
        &[]
    };

    granted.contains(&permission)
}

/// True when the permission represents a mutation (i.e. anything other than
/// pure viewing). Mutating permissions imply a state-managed-resource check.
pub fn is_mutating(permission: Permission) -> bool {
    !matches!(permission, Permission::ViewOrg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_grants_everything() {
        for p in [
            Permission::ViewOrg,
            Permission::ManageOrgSettings,
            Permission::DeleteOrg,
            Permission::ManageMembers,
            Permission::ManageWebhooks,
            Permission::EditProject,
        ] {
            assert!(role_grants(BASE_ROLE_ADMIN_ID, p), "admin missing {:?}", p);
        }
    }

    #[test]
    fn write_excludes_admin_only() {
        assert!(!role_grants(BASE_ROLE_WRITE_ID, Permission::ManageMembers));
        assert!(!role_grants(BASE_ROLE_WRITE_ID, Permission::DeleteOrg));
        assert!(!role_grants(BASE_ROLE_WRITE_ID, Permission::ManageOrgSettings));
        assert!(role_grants(BASE_ROLE_WRITE_ID, Permission::EditProject));
        assert!(role_grants(BASE_ROLE_WRITE_ID, Permission::ManageWebhooks));
    }

    #[test]
    fn view_cannot_edit_projects_or_webhooks() {
        assert!(!role_grants(BASE_ROLE_VIEW_ID, Permission::EditProject));
        assert!(!role_grants(BASE_ROLE_VIEW_ID, Permission::ManageWebhooks));
        assert!(!role_grants(BASE_ROLE_VIEW_ID, Permission::ManageMembers));
        assert!(role_grants(BASE_ROLE_VIEW_ID, Permission::ViewOrg));
    }

    #[test]
    fn unknown_role_grants_nothing() {
        let unknown = RoleId::new(uuid::uuid!("99999999-9999-9999-9999-999999999999"));
        assert!(!role_grants(unknown, Permission::ViewOrg));
        assert!(!role_grants(unknown, Permission::EditProject));
    }

    #[test]
    fn view_org_is_not_mutating() {
        assert!(!is_mutating(Permission::ViewOrg));
        assert!(is_mutating(Permission::EditProject));
        assert!(is_mutating(Permission::ManageMembers));
    }
}
