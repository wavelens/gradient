/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{EntityLookup, ErrorCollector};

pub(super) fn validate(lookup: &EntityLookup, errors: &mut ErrorCollector) {
    for worker in lookup.config.workers.values() {
        if worker.organizations.is_empty() {
            errors.push(
                format!("workers.{}.organizations", worker.worker_id),
                "Worker must be registered under at least one organization",
            );
        }
        for org in &worker.organizations {
            if !lookup.org_exists(org) {
                errors.push(
                    format!("workers.{}.organizations", worker.worker_id),
                    format!("Organization '{}' does not exist", org),
                );
            }
        }
        if !lookup.user_exists(&worker.created_by) {
            errors.push(
                format!("workers.{}.created_by", worker.worker_id),
                format!("User '{}' does not exist", worker.created_by),
            );
        }
    }
}
