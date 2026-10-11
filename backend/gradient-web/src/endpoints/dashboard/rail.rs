/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::rank::Tier;
use crate::error::WebResult;
use crate::helpers::ok_json;
use axum::extract::State;
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::dashboard::{
    RailCacheRow, RailProjectRow, RailTaskRow, rail_caches, rail_projects, rail_tasks,
};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, Serialize)]
pub struct RailTask {
    pub name: String,
    pub status: Option<EvaluationStatus>,
}

impl From<&RailTaskRow> for RailTask {
    fn from(task: &RailTaskRow) -> Self {
        Self {
            name: task.name.clone(),
            status: task.status,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct RailProject {
    pub name: String,
    pub display_name: String,
    pub starred: bool,
    pub tier: Tier,
    pub status: Option<EvaluationStatus>,
    pub task_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tasks: Option<Vec<RailTask>>,
}

impl RailProject {
    fn new(project: RailProjectRow, tasks: &[RailTaskRow]) -> Self {
        let tier = Tier::of(project.starred, project.recent_14d > 0, project.team);
        Self {
            tasks: (tier == Tier::StarredActive).then(|| nested_tasks(project.id, tasks)),
            name: project.name,
            display_name: project.display_name,
            starred: project.starred,
            tier,
            status: project.status,
            task_count: project.task_count,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct RailCache {
    pub name: String,
    pub display_name: String,
    pub starred: bool,
    pub nar_count: i64,
}

impl From<RailCacheRow> for RailCache {
    fn from(c: RailCacheRow) -> Self {
        Self {
            name: c.name,
            display_name: c.display_name,
            starred: c.starred,
            nar_count: c.nar_count,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Rail {
    pub projects: Vec<RailProject>,
    pub caches: Vec<RailCache>,
}

fn nested_tasks(project: ProjectId, tasks: &[RailTaskRow]) -> Vec<RailTask> {
    tasks
        .iter()
        .filter(|t| t.project == project)
        .map(RailTask::from)
        .collect()
}

pub fn build_rail(
    projects: Vec<RailProjectRow>,
    tasks: Vec<RailTaskRow>,
    caches: Vec<RailCacheRow>,
) -> Rail {
    let mut projects: Vec<RailProject> = projects
        .into_iter()
        .map(|p| RailProject::new(p, &tasks))
        .collect();
    projects.sort_by(|a, b| a.tier.cmp(&b.tier).then_with(|| a.name.cmp(&b.name)));
    Rail {
        projects,
        caches: caches.into_iter().map(RailCache::from).collect(),
    }
}

pub async fn get_rail(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<Rail>>> {
    let db = &state.web_db;
    Ok(ok_json(build_rail(
        rail_projects(db, user.id).await?,
        rail_tasks(db, user.id).await?,
        rail_caches(db, user.id).await?,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(name: &str, starred: bool, recent: i64) -> RailProjectRow {
        RailProjectRow {
            id: ProjectId::now_v7(),
            name: name.into(),
            display_name: name.into(),
            starred,
            member: true,
            team: false,
            recent_14d: recent,
            status: None,
            task_count: 2,
        }
    }

    fn cache(name: &str, starred: bool) -> RailCacheRow {
        RailCacheRow {
            name: name.into(),
            display_name: name.into(),
            starred,
            nar_count: 0,
        }
    }

    #[test]
    fn only_starred_active_projects_nest_their_tasks() {
        let both = project("both", true, 1);
        let starred = project("starred", true, 0);
        let tasks = vec![
            RailTaskRow {
                project: both.id,
                name: "hosts".into(),
                status: None,
            },
            RailTaskRow {
                project: starred.id,
                name: "home".into(),
                status: None,
            },
        ];
        let rail = build_rail(vec![starred, both], tasks, vec![]);
        assert_eq!(rail.projects[0].name, "both");
        assert_eq!(rail.projects[0].tasks.as_ref().unwrap().len(), 1);
        assert!(rail.projects[1].tasks.is_none());
    }

    #[test]
    fn projects_sort_by_tier_then_name_and_caches_keep_sql_order() {
        let projects = vec![
            project("zeta", false, 0),
            project("alpha", false, 0),
            project("starred", true, 0),
            project("active", false, 3),
            project("both", true, 1),
        ];
        let caches = vec![cache("zz", true), cache("aa", false)];
        let rail = build_rail(projects, vec![], caches);
        let names: Vec<&str> = rail.projects.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["both", "active", "starred", "alpha", "zeta"]);
        let tiers: Vec<Tier> = rail.projects.iter().map(|p| p.tier).collect();
        assert_eq!(
            tiers,
            [
                Tier::StarredActive,
                Tier::Active,
                Tier::Starred,
                Tier::Member,
                Tier::Member
            ]
        );
        let caches: Vec<(&str, bool)> = rail
            .caches
            .iter()
            .map(|c| (c.name.as_str(), c.starred))
            .collect();
        assert_eq!(caches, [("zz", true), ("aa", false)]);
    }
}
