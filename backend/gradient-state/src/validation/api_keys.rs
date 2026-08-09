/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{EntityLookup, ErrorCollector};
use gradient_db::permissions::Permission;

pub(super) fn validate(lookup: &EntityLookup, errors: &mut ErrorCollector) {
    for api_key in lookup.config.api_keys.values() {
        if !lookup.user_exists(&api_key.owned_by) {
            errors.push(
                format!("api_keys.{}.owned_by", api_key.name),
                format!("User '{}' does not exist", api_key.owned_by),
            );
        }
        if api_key.permissions.is_empty() {
            errors.push(
                format!("api_keys.{}.permissions", api_key.name),
                "At least one permission must be declared.",
            );
        }
        for wire in &api_key.permissions {
            if Permission::from_wire_name(wire).is_none() {
                errors.push(
                    format!("api_keys.{}.permissions", api_key.name),
                    format!("Unknown permission '{}'", wire),
                );
            }
        }
        if let Some(project) = &api_key.project
            && !lookup.project_exists(project)
        {
            errors.push(
                format!("api_keys.{}.project", api_key.name),
                format!("Project '{}' does not exist", project),
            );
        }
    }
}
