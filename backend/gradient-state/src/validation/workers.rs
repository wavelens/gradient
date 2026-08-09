/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{EntityLookup, ErrorCollector};

pub(super) fn validate(lookup: &EntityLookup, errors: &mut ErrorCollector) {
    for worker in lookup.config.workers.values() {
        if !worker.base_worker && worker.projects.is_empty() {
            errors.push(
                format!("workers.{}.projects", worker.worker_id),
                "Worker must be registered under at least one project",
            );
        }

        if worker.base_worker
            && let Some(identity) = &worker.authorize_against
            && uuid::Uuid::parse_str(identity).is_err()
        {
            errors.push(
                format!("workers.{}.authorize_against", worker.worker_id),
                format!("authorize_against '{}' is not a valid UUID", identity),
            );
        }

        for project in &worker.projects {
            if !lookup.project_exists(project) {
                errors.push(
                    format!("workers.{}.projects", worker.worker_id),
                    format!("Project '{}' does not exist", project),
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
