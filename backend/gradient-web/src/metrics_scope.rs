/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::WebError;
use crate::resource::Resource;
use gradient_pool::ProjectAccess;
use gradient_types::MUser;
use sea_orm::ConnectionTrait;
use uuid::Uuid;

#[derive(Clone)]
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
        for id in gradient_db::projects::public_ids(db).await? {
            projects.push(id.to_string());
        }
        if let Some(u) = user {
            for id in gradient_db::projects::ids_reachable_by(db, u.id).await? {
                projects.push(id.to_string());
            }
        }

        projects.sort();
        projects.dedup();
        Ok(MetricsScope::Projects(projects))
    }

    pub async fn recheck_metrics_scope_still_visible(
        &self,
        db: &impl ConnectionTrait,
        user: &Option<MUser>,
    ) -> Result<(), WebError> {
        if self.some_admitted_projects_are_no_longer_visible(&Self::resolve(db, user).await?) {
            return Err(WebError::not_found(Resource::Project));
        }
        Ok(())
    }

    fn some_admitted_projects_are_no_longer_visible(&self, current: &MetricsScope) -> bool {
        match (self, current) {
            (_, MetricsScope::All) => false,
            (MetricsScope::All, MetricsScope::Projects(_)) => true,
            (MetricsScope::Projects(admitted), MetricsScope::Projects(current)) => admitted
                .iter()
                .any(|project| current.binary_search(project).is_err()),
        }
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
    pub fn worker_projects(&self, project_access: &ProjectAccess) -> Option<Vec<Uuid>> {
        let ProjectAccess::OnlyListedProjects(peers) = project_access else {
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
    use gradient_types::ids::ProjectId;
    use std::collections::HashSet;

    #[test]
    fn a_worker_is_seen_through_any_project_it_serves_and_names_only_those() {
        let mine = ProjectId::now_v7();
        let other = ProjectId::now_v7();
        let peers = ProjectAccess::OnlyListedProjects(HashSet::from([mine, other]));
        let member = MetricsScope::Projects(vec![Uuid::from(mine).to_string()]);

        assert_eq!(member.worker_projects(&peers), Some(vec![Uuid::from(mine)]));
        assert_eq!(
            MetricsScope::Projects(vec![Uuid::now_v7().to_string()]).worker_projects(&peers),
            None
        );
    }

    #[test]
    fn project_set_that_lost_projects_narrows_admitted_metrics_scope() {
        let mut ids: Vec<String> = (0..3).map(|_| Uuid::now_v7().to_string()).collect();
        ids.sort();
        let admitted = MetricsScope::Projects(ids[..2].to_vec());

        assert!(
            admitted.some_admitted_projects_are_no_longer_visible(&MetricsScope::Projects(
                ids[..1].to_vec()
            ))
        );
        assert!(
            admitted.some_admitted_projects_are_no_longer_visible(&MetricsScope::Projects(
                ids[1..].to_vec()
            ))
        );
        assert!(
            !admitted
                .some_admitted_projects_are_no_longer_visible(&MetricsScope::Projects(ids.clone()))
        );
        assert!(
            !admitted.some_admitted_projects_are_no_longer_visible(&MetricsScope::Projects(
                ids[..2].to_vec()
            ))
        );
        assert!(!admitted.some_admitted_projects_are_no_longer_visible(&MetricsScope::All));
        assert!(
            MetricsScope::All
                .some_admitted_projects_are_no_longer_visible(&MetricsScope::Projects(ids))
        );
    }

    #[test]
    fn an_open_mode_worker_is_superuser_only() {
        assert_eq!(
            MetricsScope::All.worker_projects(&ProjectAccess::AllProjects),
            Some(vec![])
        );
        assert_eq!(
            MetricsScope::Projects(vec![]).worker_projects(&ProjectAccess::AllProjects),
            None
        );
    }
}
