/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{EntityLookup, ErrorCollector};
use crate::config::{
    EXTERNAL_HTTP_UPSTREAM_CACHE_CREDENTIAL_FIELD_COMBINATION_ERROR_MESSAGE, StateUpstream,
};
use gradient_db::caches::upstream::MAX_UPSTREAM_CACHE_COUNT;
use gradient_types::input::check_index_name;
use gradient_types::permissions::CachePermission;
use gradient_util::upstream_cache_credential::trim_and_validate_basic_auth_user_name;
use std::collections::HashSet;

pub(super) fn validate(lookup: &EntityLookup, errors: &mut ErrorCollector) {
    for cache in lookup.config.caches.values() {
        errors.record_input_error_at_field(
            format!("caches.{}.name", cache.name),
            check_index_name(&cache.name),
        );
        if !lookup.creator_is_blank_or_user_exists(&cache.created_by) {
            errors.push(
                format!("caches.{}.created_by", cache.name),
                format!("User '{}' does not exist", cache.created_by),
            );
        }

        for subscription in &cache.projects {
            if !lookup.project_exists(&subscription.name) {
                errors.push(
                    format!("caches.{}.projects", cache.name),
                    format!("Project '{}' does not exist", subscription.name),
                );
            }
            if subscription.signs_project_outputs()
                && !subscription.mode.writes_build_outputs_to_cache()
            {
                errors.push(
                    format!(
                        "caches.{}.projects.{}.sign_outputs",
                        cache.name, subscription.name
                    ),
                    "A ReadOnly subscription can't sign outputs. Change the mode to ReadWrite first.",
                );
            }
        }

        let mut role_names_seen: HashSet<&str> = HashSet::new();
        for role in &cache.roles {
            if matches!(role.name.as_str(), "Admin" | "Write" | "View") {
                errors.push(
                    format!("caches.{}.roles.{}.name", cache.name, role.name),
                    format!(
                        "Role name '{}' collides with a built-in cache role; pick a different name.",
                        role.name
                    ),
                );
            }
            if role.permissions.is_empty() {
                errors.push(
                    format!("caches.{}.roles.{}.permissions", cache.name, role.name),
                    "At least one permission must be declared.",
                );
            }
            for wire in &role.permissions {
                if CachePermission::from_wire_name(wire).is_none() {
                    errors.push(
                        format!("caches.{}.roles.{}.permissions", cache.name, role.name),
                        format!("Unknown cache permission '{}'", wire),
                    );
                }
            }
            if !role_names_seen.insert(role.name.as_str()) {
                errors.push(
                    format!("caches.{}.roles.{}.name", cache.name, role.name),
                    format!(
                        "Duplicate role name '{}' in cache '{}'",
                        role.name, cache.name
                    ),
                );
            }
        }

        if cache.upstream_caches.len() > MAX_UPSTREAM_CACHE_COUNT {
            errors.push(
                format!("caches.{}.upstream_caches", cache.name),
                format!("A cache can have at most {MAX_UPSTREAM_CACHE_COUNT} upstream caches."),
            );
        }

        for (index, upstream) in cache.upstream_caches.iter().enumerate() {
            let StateUpstream::External {
                basic_auth_user,
                basic_auth_password_file,
                bearer_token_file,
                ..
            } = upstream
            else {
                continue;
            };
            let field = format!("caches.{}.upstream_caches.{index}", cache.name);
            let basic_auth_is_complete =
                basic_auth_user.is_some() == basic_auth_password_file.is_some();
            let mixes_basic_and_bearer = basic_auth_user.is_some() && bearer_token_file.is_some();
            if !basic_auth_is_complete || mixes_basic_and_bearer {
                errors.push(
                    field,
                    EXTERNAL_HTTP_UPSTREAM_CACHE_CREDENTIAL_FIELD_COMBINATION_ERROR_MESSAGE,
                );
            } else if let Some(Err(refusal)) = basic_auth_user
                .as_deref()
                .map(trim_and_validate_basic_auth_user_name)
            {
                errors.push(field, refusal.to_string());
            }
        }

        let declared_role_names: HashSet<&str> =
            cache.roles.iter().map(|r| r.name.as_str()).collect();
        for member in &cache.members {
            if !lookup.user_exists(&member.user) {
                errors.push(
                    format!("caches.{}.members.{}.user", cache.name, member.user),
                    format!("User '{}' does not exist", member.user),
                );
            }
            let builtin = matches!(member.role.as_str(), "Admin" | "Write" | "View");
            if !builtin && !declared_role_names.contains(member.role.as_str()) {
                errors.push(
                    format!("caches.{}.members.{}.role", cache.name, member.user),
                    format!("Role '{}' not found in cache '{}'", member.role, cache.name),
                );
            }
        }
    }
}
