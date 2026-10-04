/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::WebError;
use gradient_types::MUser;
use gradient_types::ids::ProjectId;
use sea_orm::{ConnectionTrait, Value};
use std::collections::HashSet;
use uuid::Uuid;

gradient_db::sql! {
    PUBLIC_PROJECTS = "SELECT id FROM project WHERE public = true",
        params = [];

    PROJECTS_FOR_USER = "SELECT DISTINCT project AS id FROM project_access WHERE \"user\" = $1",
        params = [UserId];
}

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
        for row in db.query_all_raw(PUBLIC_PROJECTS.stmt()).await? {
            projects.push(row.try_get::<Uuid>("", "id")?.to_string());
        }
        if let Some(u) = user {
            for row in db
                .query_all_raw(PROJECTS_FOR_USER.bind([Value::from(Uuid::from(u.id))]))
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

    pub fn allows(&self, project: &Uuid) -> bool {
        match self {
            MetricsScope::All => true,
            MetricsScope::Projects(projects) => projects.contains(&project.to_string()),
        }
    }

    /// A worker with no authorization filter (open mode) is superuser-only.
    pub fn worker_projects(&self, authorized: Option<&HashSet<ProjectId>>) -> Option<Vec<Uuid>> {
        let Some(peers) = authorized else {
            return self.is_all().then(Vec::new);
        };
        let visible: Vec<Uuid> = peers
            .iter()
            .map(|&p| Uuid::from(p))
            .filter(|p| self.allows(p))
            .collect();
        (self.is_all() || !visible.is_empty()).then_some(visible)
    }

    pub fn project_ids(&self) -> Option<Vec<Uuid>> {
        match self {
            MetricsScope::All => None,
            MetricsScope::Projects(projects) => {
                Some(projects.iter().filter_map(|p| p.parse().ok()).collect())
            }
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_worker_is_seen_through_any_project_it_serves_and_names_only_those() {
        let mine = ProjectId::now_v7();
        let other = ProjectId::now_v7();
        let peers = HashSet::from([mine, other]);
        let member = MetricsScope::Projects(vec![Uuid::from(mine).to_string()]);

        assert_eq!(
            member.worker_projects(Some(&peers)),
            Some(vec![Uuid::from(mine)])
        );
        assert_eq!(
            MetricsScope::Projects(vec![Uuid::now_v7().to_string()]).worker_projects(Some(&peers)),
            None
        );
    }

    #[test]
    fn an_open_mode_worker_is_superuser_only() {
        assert_eq!(MetricsScope::All.worker_projects(None), Some(vec![]));
        assert_eq!(MetricsScope::Projects(vec![]).worker_projects(None), None);
    }
}
