/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{EntityLookup, ErrorCollector};

pub(super) fn validate(lookup: &EntityLookup, errors: &mut ErrorCollector) {
    for user in lookup.config.users.values() {
        if !user.email.contains('@') {
            errors.push(
                format!("users.{}.email", user.username),
                "Invalid email format",
            );
        }
    }
}
