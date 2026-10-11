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

#[derive(Serialize, Debug)]
pub struct PermissionEntry {
    pub id: &'static str,
    pub mutating: bool,
}

pub fn available_permissions() -> Vec<PermissionEntry> {
    Permission::ALL
        .iter()
        .copied()
        .map(|p| PermissionEntry {
            id: p.as_wire_name(),
            mutating: is_mutating(p),
        })
        .collect()
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
        .map(|p| PermissionEntry {
            id: p.as_wire_name(),
            mutating: is_cache_mutating(p),
        })
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
