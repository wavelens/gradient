/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub use gradient_types::permissions::*;

use crate::error::WebResult;
use crate::refusal::{Refusal, RefusalStatus};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
enum PermissionRefusal {
    #[error("Unknown permission '{permission}'. See {catalogue_hint} for the catalogue.")]
    UnknownPermission {
        permission: String,
        catalogue_hint: String,
    },
    #[error("Unknown cache permission '{permission}'. See {catalogue_hint} for the catalogue.")]
    UnknownCachePermission {
        permission: String,
        catalogue_hint: String,
    },
}

impl Refusal for PermissionRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::UnknownPermission { .. } | Self::UnknownCachePermission { .. } => {
                RefusalStatus::BadRequest
            }
        }
    }
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct PermissionEntry {
    pub id: &'static str,
    pub mutating: bool,
    pub permission_explanation: &'static str,
}

impl From<Permission> for PermissionEntry {
    fn from(permission: Permission) -> Self {
        Self {
            id: permission.as_wire_name(),
            mutating: is_mutating(permission),
            permission_explanation: permission.permission_explanation(),
        }
    }
}

impl From<CachePermission> for PermissionEntry {
    fn from(permission: CachePermission) -> Self {
        Self {
            id: permission.as_wire_name(),
            mutating: is_cache_mutating(permission),
            permission_explanation: permission.permission_explanation(),
        }
    }
}

pub fn available_permissions() -> Vec<PermissionEntry> {
    Permission::ALL
        .iter()
        .copied()
        .filter(|p| !p.is_server_wide())
        .map(PermissionEntry::from)
        .collect()
}

pub fn available_server_permissions() -> Vec<PermissionEntry> {
    Permission::ALL
        .iter()
        .copied()
        .filter(|p| p.is_server_wide())
        .map(PermissionEntry::from)
        .collect()
}

pub fn parse_permission_list_for_project_role(
    wire: &[String],
    catalogue_hint: &str,
) -> WebResult<PermissionMask> {
    let mask = parse_permission_list(wire, catalogue_hint)?;
    match mask_to_vec(mask & server_wide_mask()).first() {
        Some(server_wide) => Err(PermissionRefusal::UnknownPermission {
            permission: server_wide.as_wire_name().to_string(),
            catalogue_hint: catalogue_hint.to_string(),
        }
        .into()),
        None => Ok(mask),
    }
}

/// Empty input is allowed at this layer. Call sites requiring a non-empty set must check the mask
/// themselves.
pub fn parse_permission_list(wire: &[String], catalogue_hint: &str) -> WebResult<PermissionMask> {
    let mut perms = Vec::with_capacity(wire.len());
    for w in wire {
        let parsed =
            Permission::from_wire_name(w).ok_or_else(|| PermissionRefusal::UnknownPermission {
                permission: w.to_string(),
                catalogue_hint: catalogue_hint.to_string(),
            })?;
        perms.push(parsed);
    }
    Ok(mask_from(&perms))
}

pub fn available_cache_permissions() -> Vec<PermissionEntry> {
    CachePermission::ALL
        .iter()
        .copied()
        .map(PermissionEntry::from)
        .collect()
}

pub fn parse_cache_permission_list(
    wire: &[String],
    catalogue_hint: &str,
) -> WebResult<PermissionMask> {
    let mut perms = Vec::with_capacity(wire.len());
    for w in wire {
        let parsed = CachePermission::from_wire_name(w).ok_or_else(|| {
            PermissionRefusal::UnknownCachePermission {
                permission: w.to_string(),
                catalogue_hint: catalogue_hint.to_string(),
            }
        })?;
        perms.push(parsed);
    }
    Ok(cache_mask_from(&perms))
}

pub fn cache_mask_to_wire(mask: PermissionMask) -> Vec<&'static str> {
    cache_mask_to_vec(mask)
        .into_iter()
        .map(|p| p.as_wire_name())
        .collect()
}
