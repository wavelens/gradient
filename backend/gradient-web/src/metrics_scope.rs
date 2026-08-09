/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Per-request visibility envelope for the Job Board / metrics surfaces.
//!
//! Superusers see every project; members see their projects plus public projects; anonymous
//! callers see public projects only. Cross-project infrastructure data is shown to
//! non-superusers only in anonymized aggregate (see the board endpoints).

use crate::error::WebError;
use gradient_types::MUser;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement, Value};
use uuid::Uuid;

pub enum MetricsScope {
    All,
    Projects(Vec<String>),
}

impl MetricsScope {
    pub async fn resolve(
        db: &impl ConnectionTrait,
        user: &Option<MUser>,
    ) -> Result<Self, WebError> {
        if user.as_ref().is_some_and(|u| u.superuser) {
            return Ok(MetricsScope::All);
        }

        let mut projects: Vec<String> = Vec::new();
        for row in db
            .query_all(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT id FROM project WHERE public = true".to_owned(),
            ))
            .await?
        {
            projects.push(row.try_get::<Uuid>("", "id")?.to_string());
        }
        if let Some(u) = user {
            for row in db
                .query_all(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "SELECT project AS id FROM project_user WHERE \"user\" = $1",
                    [Value::from(Uuid::from(u.id))],
                ))
                .await?
            {
                projects.push(row.try_get::<Uuid>("", "id")?.to_string());
            }
        }

        projects.sort();
        projects.dedup();
        Ok(MetricsScope::Projects(projects))
    }

    pub fn is_all(&self) -> bool {
        matches!(self, MetricsScope::All)
    }

    /// True when the caller may see unmasked detail for `project`.
    pub fn allows(&self, project: &Uuid) -> bool {
        match self {
            MetricsScope::All => true,
            MetricsScope::Projects(projects) => projects.contains(&project.to_string()),
        }
    }

    /// SQL `IN (...)` fragment of accessible project UUID literals, or `None` for
    /// the unrestricted (superuser) scope. Values are DB-sourced UUIDs.
    pub fn project_in_list(&self) -> Option<String> {
        match self {
            MetricsScope::All => None,
            MetricsScope::Projects(projects) => Some(
                projects
                    .iter()
                    .map(|o| format!("'{o}'"))
                    .collect::<Vec<_>>()
                    .join(","),
            ),
        }
    }
}
