/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashSet;

use gradient_types::ids::ProjectId;
use serde::{Serialize, Serializer};

/// The worker serves no project if the `OnlyListedProjects` set is empty. Production code can never
/// construct `AllProjects`. Scheduler tests use this case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectAccess {
    AllProjects,
    OnlyListedProjects(HashSet<ProjectId>),
}

impl ProjectAccess {
    pub fn no_project_access() -> Self {
        Self::OnlyListedProjects(HashSet::new())
    }

    pub fn contains(&self, id: &ProjectId) -> bool {
        match self {
            Self::AllProjects => true,
            Self::OnlyListedProjects(projects) => projects.contains(id),
        }
    }

    pub fn allows_empty_project_set(&self) -> bool {
        match self {
            Self::AllProjects => false,
            Self::OnlyListedProjects(projects) => projects.is_empty(),
        }
    }
}

impl Serialize for ProjectAccess {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::AllProjects => serializer.serialize_none(),
            Self::OnlyListedProjects(projects) => serializer.serialize_some(projects),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_only_projects_contains_no_project_and_equals_no_project() {
        let access = ProjectAccess::OnlyListedProjects(HashSet::new());
        assert!(!access.contains(&ProjectId::now_v7()));
        assert!(access.allows_empty_project_set());
        assert_eq!(access, ProjectAccess::no_project_access());
    }

    #[test]
    fn only_projects_set_contains_listed_project_and_no_other_project() {
        let listed = ProjectId::now_v7();
        let access = ProjectAccess::OnlyListedProjects(HashSet::from([listed]));
        assert!(access.contains(&listed));
        assert!(!access.contains(&ProjectId::now_v7()));
        assert!(!access.allows_empty_project_set());
    }

    #[test]
    fn every_project_contains_any_project_id() {
        let access = ProjectAccess::AllProjects;
        assert!(access.contains(&ProjectId::now_v7()));
        assert!(!access.allows_empty_project_set());
    }

    #[test]
    fn project_access_serializes_every_project_as_null_and_only_projects_as_id_list() {
        let listed = ProjectId::now_v7();
        assert_eq!(
            serde_json::to_value(ProjectAccess::AllProjects).unwrap(),
            serde_json::Value::Null
        );
        assert_eq!(
            serde_json::to_value(ProjectAccess::no_project_access()).unwrap(),
            serde_json::json!([])
        );
        assert_eq!(
            serde_json::to_value(ProjectAccess::OnlyListedProjects(HashSet::from([listed])))
                .unwrap(),
            serde_json::json!([listed])
        );
    }
}
