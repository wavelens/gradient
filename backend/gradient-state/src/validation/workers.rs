/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{EntityLookup, ErrorCollector};

pub(super) fn validate(lookup: &EntityLookup, errors: &mut ErrorCollector) {
    for worker in lookup.config.workers.values() {
        match (&worker.team, worker.projects.is_empty()) {
            (Some(_), false) => errors.push(
                format!("workers.{}.team", worker.worker_id),
                "A worker belongs to a team or to projects, not both",
            ),
            (None, true) => errors.push(
                format!("workers.{}.projects", worker.worker_id),
                "Worker must belong to a team or be registered under at least one project",
            ),
            _ => {}
        }
        if let Some(team) = &worker.team
            && !lookup.team_exists(team)
        {
            errors.push(
                format!("workers.{}.team", worker.worker_id),
                format!("Team '{}' does not exist", team),
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
        if let Some(created_by) = &worker.created_by
            && !lookup.user_exists(created_by)
        {
            errors.push(
                format!("workers.{}.created_by", worker.worker_id),
                format!("User '{}' does not exist", created_by),
            );
        }
    }
}
