/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::DynError;
use super::StateApplicator;
use gradient_ci::IntegrationKind;
use gradient_entity::*;
use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use std::collections::HashMap;

pub(crate) fn parse_integration_kind(kind: &str) -> Option<IntegrationKind> {
    match kind {
        "inbound" => Some(IntegrationKind::Inbound),
        "outbound" => Some(IntegrationKind::Outbound),
        _ => None,
    }
}

pub(crate) fn lookup_id<T: Copy>(
    map: &HashMap<String, T>,
    name: &str,
    kind: &str,
) -> Result<T, DynError> {
    map.get(name)
        .copied()
        .ok_or_else(|| format!("{} '{}' not found", kind, name).into())
}

/// The GitHub App is seeding two integrations per project sharing `name = "github"`, one inbound
/// and one outbound. A collect ignoring `kind` is collapsing them into one arbitrary entry. The
/// webhook resolver then misses the outbound id and the trigger never fires.
pub(crate) async fn inbound_integrations_by_name<C: ConnectionTrait>(
    db: &C,
    project_id: ProjectId,
) -> Result<HashMap<String, IntegrationId>, sea_orm::DbErr> {
    Ok(integration::Entity::find()
        .filter(integration::Column::Project.eq(project_id))
        .filter(integration::Column::Kind.eq(i16::from(IntegrationKind::Inbound)))
        .all(db)
        .await?
        .into_iter()
        .map(|r| (r.name, r.id))
        .collect())
}

pub(crate) async fn outbound_integrations_by_name<C: ConnectionTrait>(
    db: &C,
    project_id: ProjectId,
) -> Result<HashMap<String, IntegrationId>, sea_orm::DbErr> {
    Ok(integration::Entity::find()
        .filter(integration::Column::Project.eq(project_id))
        .filter(integration::Column::Kind.eq(i16::from(IntegrationKind::Outbound)))
        .all(db)
        .await?
        .into_iter()
        .map(|r| (r.name, r.id))
        .collect())
}

impl<'a> StateApplicator<'a> {
    pub(crate) async fn user_lookup(&self) -> Result<HashMap<String, UserId>, DynError> {
        let users = user::Entity::find().all(self.db).await?;
        Ok(users.into_iter().map(|u| (u.username, u.id)).collect())
    }

    pub(crate) async fn project_lookup(&self) -> Result<HashMap<String, ProjectId>, DynError> {
        let projects = project::Entity::find().all(self.db).await?;
        Ok(projects.into_iter().map(|o| (o.name, o.id)).collect())
    }
}

#[cfg(test)]
mod inbound_integration_lookup_tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use uuid::Uuid;

    pub(crate) fn project_id() -> ProjectId {
        ProjectId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap())
    }

    #[tokio::test]
    async fn inbound_integrations_lookup_sql_filters_kind_inbound() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<integration::Model>::new()])
            .into_connection();

        inbound_integrations_by_name(&db, project_id())
            .await
            .unwrap();

        let logs = db.into_transaction_log();
        let select = logs
            .iter()
            .flat_map(|t| t.statements())
            .find(|s| s.sql.to_lowercase().contains("from \"integration\""))
            .expect("expected a SELECT FROM integration statement");

        assert!(
            select.sql.contains("\"kind\""),
            "SELECT must filter by kind column: {}",
            select.sql,
        );

        let inbound = i16::from(IntegrationKind::Inbound);
        let bound: Vec<sea_orm::Value> = select
            .values
            .as_ref()
            .map(|v| v.0.clone())
            .unwrap_or_default();
        assert!(
            bound
                .iter()
                .any(|v| matches!(v, sea_orm::Value::SmallInt(Some(n)) if *n == inbound)),
            "SELECT must bind inbound kind as SmallInt({inbound}); bound values: {bound:?}",
        );
    }
}
