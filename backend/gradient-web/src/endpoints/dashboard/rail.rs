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
    RailCacheRow, RailProjectRow, RailTaskRow, has_project_workers, rail_caches, rail_projects,
    rail_tasks,
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

#[derive(Debug, Serialize)]
pub struct RailCache {
    pub name: String,
    pub display_name: String,
    pub starred: bool,
    pub nar_count: i64,
}

#[derive(Debug, Serialize)]
pub struct Rail {
    pub projects: Vec<RailProject>,
    pub caches: Vec<RailCache>,
    pub operations: bool,
}

fn nested_tasks(project: ProjectId, tasks: &[RailTaskRow]) -> Vec<RailTask> {
    tasks
        .iter()
        .filter(|t| t.project == project)
        .map(|t| RailTask {
            name: t.name.clone(),
            status: t.status,
        })
        .collect()
}

fn rail_project(p: RailProjectRow, tasks: &[RailTaskRow]) -> RailProject {
    let tier = Tier::of(p.starred, p.recent_14d > 0);
    RailProject {
        tasks: (tier == Tier::StarredActive).then(|| nested_tasks(p.id, tasks)),
        name: p.name,
        display_name: p.display_name,
        starred: p.starred,
        tier,
        status: p.status,
        task_count: p.task_count,
    }
}

fn rail_cache(c: RailCacheRow) -> RailCache {
    RailCache {
        name: c.name,
        display_name: c.display_name,
        starred: c.starred,
        nar_count: c.nar_count,
    }
}

pub fn build_rail(
    projects: Vec<RailProjectRow>,
    tasks: Vec<RailTaskRow>,
    caches: Vec<RailCacheRow>,
    operations: bool,
) -> Rail {
    let mut projects: Vec<RailProject> = projects
        .into_iter()
        .map(|p| rail_project(p, &tasks))
        .collect();
    projects.sort_by(|a, b| a.tier.cmp(&b.tier).then_with(|| a.name.cmp(&b.name)));
    Rail {
        projects,
        caches: caches.into_iter().map(rail_cache).collect(),
        operations,
    }
}

pub async fn get_rail(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<Rail>>> {
    let db = &state.web_db;
    let operations = user.superuser || has_project_workers(db, user.id).await?;
    Ok(ok_json(build_rail(
        rail_projects(db, user.id).await?,
        rail_tasks(db, user.id).await?,
        rail_caches(db, user.id).await?,
        operations,
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
        let rail = build_rail(vec![starred, both], tasks, vec![], false);
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
        let rail = build_rail(projects, vec![], caches, true);
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
        assert!(rail.operations);
    }
}
