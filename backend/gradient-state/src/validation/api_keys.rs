/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{EntityLookup, ErrorCollector};
use gradient_types::permissions::Permission;

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
            match Permission::from_wire_name(wire) {
                None => errors.push(
                    format!("api_keys.{}.permissions", api_key.name),
                    format!("Unknown permission '{}'", wire),
                ),
                Some(permission) if permission.is_server_wide() && api_key.project.is_some() => {
                    errors.push(
                        format!("api_keys.{}.permissions", api_key.name),
                        format!(
                            "Permission '{}' can apply to the whole server. Remove the project pin or the permission.",
                            wire
                        ),
                    )
                }
                Some(Permission::UseSuperuserRights)
                    if lookup.user_exists(&api_key.owned_by)
                        && !lookup.user_is_superuser(&api_key.owned_by) =>
                {
                    errors.push(
                        format!("api_keys.{}.permissions", api_key.name),
                        format!(
                            "Permission 'useSuperuserRights' can apply only to an owner with superuser rights. Make user '{}' a superuser, or remove this permission.",
                            api_key.owned_by
                        ),
                    )
                }
                Some(_) => {}
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
