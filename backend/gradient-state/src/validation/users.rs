/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{EntityLookup, ErrorCollector};
use gradient_types::input::{validate_display_name, validate_email, validate_username_syntax};
use std::collections::HashSet;

pub(super) fn validate(lookup: &EntityLookup, errors: &mut ErrorCollector) {
    let mut seen_lowercased_declared_usernames: HashSet<String> = HashSet::new();
    let mut seen_lowercase_declared_emails: HashSet<String> = HashSet::new();
    let mut seen_existing_account_ids: HashSet<uuid::Uuid> = HashSet::new();
    for user in lookup.config.users.values() {
        let field = |key: &str| format!("users.{}.{key}", user.username);
        errors.record_input_error_at_field(
            field("username"),
            validate_username_syntax(&user.username),
        );
        errors.record_input_error_at_field(field("name"), validate_display_name(&user.name));
        errors.record_input_error_at_field(field("email"), validate_email(&user.email));

        if !seen_lowercased_declared_usernames.insert(user.username.to_lowercase()) {
            errors.push(
                field("username"),
                format!(
                    "Duplicate username '{}' (username comparison is case-insensitive)",
                    user.username
                ),
            );
        }
        if let Some(id) = &user.existing_account_id {
            match id.trim().parse::<uuid::Uuid>() {
                Ok(parsed) if !seen_existing_account_ids.insert(parsed) => errors.push(
                    field("existing_account_id"),
                    format!("existing_account_id '{id}' already in use by another declared user"),
                ),
                Ok(_) => {}
                Err(_) => errors.push(field("existing_account_id"), format!("Invalid UUID '{id}'")),
            }
        }
        if !seen_lowercase_declared_emails.insert(user.email.to_lowercase()) {
            errors.push(
                field("email"),
                format!(
                    "Another declared user already used the email address '{}'",
                    user.email
                ),
            );
        }
    }
}
