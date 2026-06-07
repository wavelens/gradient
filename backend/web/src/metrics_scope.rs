/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Per-request visibility envelope for the Job Board / metrics surfaces.
//!
//! Superusers see every org; members see their orgs plus public orgs; anonymous
//! callers see public orgs only. Cross-org infrastructure data is shown to
//! non-superusers only in anonymized aggregate (see the board endpoints).

use crate::error::WebError;
use gradient_core::types::MUser;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement, Value};
use uuid::Uuid;

pub enum MetricsScope {
    All,
    Orgs(Vec<String>),
}

impl MetricsScope {
    pub async fn resolve(
        db: &impl ConnectionTrait,
        user: &Option<MUser>,
    ) -> Result<Self, WebError> {
        if user.as_ref().is_some_and(|u| u.superuser) {
            return Ok(MetricsScope::All);
        }

        let mut orgs: Vec<String> = Vec::new();
        for row in db
            .query_all(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT id FROM organization WHERE public = true".to_owned(),
            ))
            .await?
        {
            orgs.push(row.try_get::<Uuid>("", "id")?.to_string());
        }
        if let Some(u) = user {
            for row in db
                .query_all(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "SELECT organization AS id FROM organization_user WHERE \"user\" = $1",
                    [Value::from(Uuid::from(u.id))],
                ))
                .await?
            {
                orgs.push(row.try_get::<Uuid>("", "id")?.to_string());
            }
        }

        orgs.sort();
        orgs.dedup();
        Ok(MetricsScope::Orgs(orgs))
    }

    pub fn is_all(&self) -> bool {
        matches!(self, MetricsScope::All)
    }

    /// True when the caller may see unmasked detail for `org`.
    pub fn allows(&self, org: &Uuid) -> bool {
        match self {
            MetricsScope::All => true,
            MetricsScope::Orgs(orgs) => orgs.contains(&org.to_string()),
        }
    }

    /// SQL `IN (...)` fragment of accessible org UUID literals, or `None` for
    /// the unrestricted (superuser) scope. Values are DB-sourced UUIDs.
    pub fn org_in_list(&self) -> Option<String> {
        match self {
            MetricsScope::All => None,
            MetricsScope::Orgs(orgs) => Some(
                orgs.iter()
                    .map(|o| format!("'{o}'"))
                    .collect::<Vec<_>>()
                    .join(","),
            ),
        }
    }
}
