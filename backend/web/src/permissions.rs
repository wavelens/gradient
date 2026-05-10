/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Re-export of the org permission system, plus web-layer helpers shared
//! between the role-management API and the API-key endpoints. The canonical
//! capability set lives in [`gradient_core::permissions`].

pub use gradient_core::permissions::*;

use crate::error::{WebError, WebResult};
use serde::Serialize;

#[derive(Serialize, Debug)]
pub struct PermissionEntry {
    pub id: &'static str,
    pub mutating: bool,
}

/// Catalogue of every capability a custom role or API key may grant. Used by
/// both the role-management UI (`GET /orgs/{org}/roles`) and the API-key UI
/// (`GET /user/keys/permissions`).
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

/// Parse a list of wire-format permission identifiers into a [`PermissionMask`].
/// `catalogue_hint` appears in the error message so callers can guide users to
/// the right `available_permissions` endpoint.
///
/// Empty input is allowed at this layer (matches role semantics); call sites
/// that require non-empty (e.g. API-key creation) must check the resulting
/// mask themselves.
pub fn parse_permission_list(
    wire: &[String],
    catalogue_hint: &str,
) -> WebResult<PermissionMask> {
    let mut perms = Vec::with_capacity(wire.len());
    for w in wire {
        let parsed = Permission::from_wire_name(w).ok_or_else(|| {
            WebError::bad_request(format!(
                "Unknown permission '{}'. See {} for the catalogue.",
                w, catalogue_hint
            ))
        })?;
        perms.push(parsed);
    }
    Ok(mask_from(&perms))
}
