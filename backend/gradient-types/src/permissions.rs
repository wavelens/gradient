/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::consts::{BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID, BASE_ROLE_WRITE_ID};
use crate::ids::{CacheId, RoleId};
pub use gradient_api_types::{
    CachePermission, Permission, PermissionMask, is_cache_mutating, is_mutating,
};

#[inline]
pub const fn mask_grants(mask: PermissionMask, permission: Permission) -> bool {
    mask & permission.bit() != 0
}

pub const fn api_key_without_cache_pin_can_read_project_caches(
    key_project_mask: PermissionMask,
    cache_pin: Option<CacheId>,
) -> bool {
    cache_pin.is_none() && mask_grants(key_project_mask, Permission::ViewProject)
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

pub fn server_wide_mask() -> PermissionMask {
    Permission::ALL
        .iter()
        .filter(|p| p.is_server_wide())
        .fold(0, |acc, p| acc | p.bit())
}

pub fn superuser_permissions_mask() -> PermissionMask {
    mask_from(&[Permission::ReadSuperuser, Permission::WriteSuperuser])
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
        OpenJobTerminal,
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
    fn test_api_key_pin_and_view_project_determine_subscribed_cache_read_access() {
        let view = mask_from(&[Permission::ViewProject]);
        let trigger_only = mask_from(&[Permission::TriggerEvaluation]);
        let pin = Some(CacheId::now_v7());
        for (mask, cache_pin, expected) in [
            (view, None, true),
            (PermissionMask::MAX, None, true),
            (trigger_only, None, false),
            (0, None, false),
            (view, pin, false),
            (0, pin, false),
        ] {
            assert_eq!(
                api_key_without_cache_pin_can_read_project_caches(mask, cache_pin),
                expected,
                "{mask:#x} {cache_pin:?}"
            );
        }
    }

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
        assert!(mask_grants(write, Permission::OpenJobTerminal));
        assert!(!mask_grants(
            mask_from(&[Permission::ViewProject, Permission::TriggerEvaluation]),
            Permission::OpenJobTerminal
        ));

        let view = view_mask();
        assert!(!mask_grants(view, Permission::EditTask));
        assert!(!mask_grants(view, Permission::ManageActions));
        assert!(!mask_grants(view, Permission::ManageMembers));
        assert!(!mask_grants(view, Permission::ManageRoles));
        assert!(!mask_grants(view, Permission::OpenJobTerminal));
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

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig {
            failure_persistence: None,
            ..proptest::prelude::ProptestConfig::default()
        })]

        #[test]
        fn permission_masks_round_trip_through_vectors(
            mask in proptest::prelude::any::<i64>(),
            granted in proptest::sample::subsequence(Permission::ALL, 0..=Permission::ALL.len()),
            cache_granted in proptest::sample::subsequence(
                CachePermission::ALL,
                0..=CachePermission::ALL.len(),
            ),
        ) {
            proptest::prop_assert_eq!(
                mask_from(&mask_to_vec(mask)),
                mask & mask_from(Permission::ALL)
            );
            proptest::prop_assert_eq!(
                cache_mask_from(&cache_mask_to_vec(mask)),
                mask & cache_mask_from(CachePermission::ALL)
            );
            proptest::prop_assert_eq!(mask_to_vec(mask_from(&granted)), granted);
            proptest::prop_assert_eq!(
                cache_mask_to_vec(cache_mask_from(&cache_granted)),
                cache_granted
            );
        }
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
