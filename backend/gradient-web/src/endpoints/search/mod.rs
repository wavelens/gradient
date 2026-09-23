/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod classify;

use crate::error::WebResult;
use crate::helpers::ok_json;
use axum::extract::{Query, State};
use axum::{Extension, Json};
use classify::{Lookups, classify};
use gradient_core::ServerState;
use gradient_db::dashboard::{
    CommitHitRow, NameHitRow, NameKind, NarHitRow, StarredNames, search_commits, search_names,
    search_nars, starred_names,
};
use gradient_types::*;
use sea_orm::{ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    pub q: String,
    pub limit: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HitKind {
    Project,
    Task,
    Cache,
    Nar,
    Commit,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct SearchHit {
    pub kind: HitKind,
    pub label: String,
    pub sublabel: String,
    pub route: String,
    pub starred: bool,
}

fn per_kind(limit: Option<u64>) -> u64 {
    limit.unwrap_or(5).clamp(1, 20)
}

fn project_hit(name: String, starred: bool) -> SearchHit {
    SearchHit {
        kind: HitKind::Project,
        route: format!("/project/{name}"),
        label: name,
        sublabel: String::new(),
        starred,
    }
}

fn task_hit(project: String, task: String, starred: bool) -> SearchHit {
    SearchHit {
        kind: HitKind::Task,
        route: format!("/project/{project}/task/{task}"),
        label: task,
        sublabel: project,
        starred,
    }
}

fn cache_hit(name: String, starred: bool) -> SearchHit {
    SearchHit {
        kind: HitKind::Cache,
        route: format!("/caches/{name}"),
        label: name,
        sublabel: String::new(),
        starred,
    }
}

fn starred_hits(s: StarredNames) -> Vec<SearchHit> {
    let projects = s.projects.into_iter().map(|p| project_hit(p, true));
    let tasks = s
        .tasks
        .into_iter()
        .map(|t| task_hit(t.project, t.task, true));
    let caches = s.caches.into_iter().map(|c| cache_hit(c, true));
    projects.chain(tasks).chain(caches).collect()
}

fn nar_hit(n: NarHitRow) -> SearchHit {
    SearchHit {
        kind: HitKind::Nar,
        route: format!("/caches/{}/nars?hash={}", n.cache, n.hash),
        label: n.package,
        sublabel: n.cache,
        starred: false,
    }
}

fn commit_hit(c: CommitHitRow) -> SearchHit {
    SearchHit {
        kind: HitKind::Commit,
        route: format!(
            "/project/{}/task/{}?eval={}",
            c.project, c.task, c.evaluation
        ),
        label: c.hash.chars().take(7).collect(),
        sublabel: format!("{} / {}", c.project, c.task),
        starred: false,
    }
}

fn name_hit(n: NameHitRow) -> SearchHit {
    let mut hit = match (n.kind, n.project) {
        (NameKind::Task, Some(project)) => task_hit(project, n.name, n.starred),
        (NameKind::Cache, _) => cache_hit(n.name, n.starred),
        _ => project_hit(n.name, n.starred),
    };
    hit.label = n.display_name;
    hit
}

pub async fn search<C: ConnectionTrait>(
    db: &C,
    user: UserId,
    superuser: bool,
    lookups: Lookups,
    per_kind: u64,
) -> Result<Vec<SearchHit>, DbErr> {
    let Some(text) = lookups.text else {
        return Ok(starred_hits(starred_names(db, user, superuser).await?));
    };
    let mut hits = Vec::new();
    if let Some(hash) = &lookups.nar {
        let nars = search_nars(db, hash, user, superuser).await?;
        hits.extend(nars.into_iter().map(nar_hit));
    }
    if let Some(range) = &lookups.commit {
        let commits = search_commits(db, &range.low, &range.high, user, superuser).await?;
        hits.extend(commits.into_iter().map(commit_hit));
    }
    let names = search_names(db, &text, per_kind, user, superuser).await?;
    hits.extend(names.into_iter().map(name_hit));
    Ok(hits)
}

pub async fn get_search(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Query(q): Query<SearchQuery>,
) -> WebResult<Json<BaseResponse<Vec<SearchHit>>>> {
    let hits = search(
        &state.web_db,
        user.id,
        user.superuser,
        classify(&q.q),
        per_kind(q.limit),
    )
    .await?;
    Ok(ok_json(hits))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, Value};
    use std::collections::BTreeMap;

    type Row = BTreeMap<&'static str, Value>;

    const HASH: &str = "yg73zdgp9fbq8z14cnjxcy1507ac2d6r";

    fn name_row(kind: &str, project: Option<&str>, name: &str, starred: bool) -> Row {
        BTreeMap::from([
            ("kind", Value::from(kind)),
            ("project", Value::String(project.map(str::to_string))),
            ("name", Value::from(name)),
            ("display_name", Value::from(name.to_uppercase())),
            ("starred", Value::from(starred)),
        ])
    }

    fn routes(hits: &[SearchHit]) -> Vec<(HitKind, &str)> {
        hits.iter().map(|h| (h.kind, h.route.as_str())).collect()
    }

    async fn run(results: Vec<Vec<Row>>, q: &str) -> (Vec<SearchHit>, usize) {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(results)
            .into_connection();
        let hits = search(&db, UserId::now_v7(), false, classify(q), 5)
            .await
            .unwrap();
        (hits, db.into_transaction_log().len())
    }

    #[test]
    fn limit_is_clamped() {
        assert_eq!(per_kind(None), 5);
        assert_eq!(per_kind(Some(0)), 1);
        assert_eq!(per_kind(Some(999)), 20);
    }

    #[tokio::test]
    async fn an_empty_query_lists_starred_items() {
        let (hits, statements) = run(
            vec![
                vec![BTreeMap::from([("name", Value::from("infra"))])],
                vec![BTreeMap::from([
                    ("project", Value::from("infra")),
                    ("name", Value::from("hosts")),
                ])],
                vec![BTreeMap::from([("name", Value::from("main"))])],
            ],
            "  ",
        )
        .await;

        assert_eq!(
            routes(&hits),
            [
                (HitKind::Project, "/project/infra"),
                (HitKind::Task, "/project/infra/task/hosts"),
                (HitKind::Cache, "/caches/main"),
            ]
        );
        assert!(hits.iter().all(|h| h.starred));
        assert_eq!(statements, 3);
    }

    #[tokio::test]
    async fn name_hits_route_per_kind() {
        let (hits, statements) = run(
            vec![vec![
                name_row("task", Some("infra"), "hosts", true),
                name_row("cache", None, "main", false),
                name_row("project", None, "infra", false),
            ]],
            "hello",
        )
        .await;

        assert_eq!(
            routes(&hits),
            [
                (HitKind::Task, "/project/infra/task/hosts"),
                (HitKind::Cache, "/caches/main"),
                (HitKind::Project, "/project/infra"),
            ]
        );
        assert_eq!(hits[0].label, "HOSTS");
        assert_eq!(hits[0].sublabel, "infra");
        assert!(hits[0].starred);
        assert_eq!(statements, 1);
    }

    #[tokio::test]
    async fn a_nar_hash_routes_to_the_cache_nar_listing() {
        let nar = BTreeMap::from([
            ("cache", Value::from("main")),
            ("hash", Value::from(HASH)),
            ("package", Value::from("hello-2.12.1")),
        ]);
        let (hits, statements) = run(vec![vec![nar], vec![]], HASH).await;

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, HitKind::Nar);
        assert_eq!(hits[0].route, format!("/caches/main/nars?hash={HASH}"));
        assert_eq!(hits[0].label, "hello-2.12.1");
        assert_eq!(statements, 2);
    }

    #[tokio::test]
    async fn a_hex_name_finds_commits_and_names() {
        let evaluation = uuid::Uuid::now_v7();
        let commit = BTreeMap::from([
            ("project", Value::from("infra")),
            ("task", Value::from("hosts")),
            ("evaluation", Value::Uuid(Some(evaluation))),
            ("hash", Value::from(format!("deadbeef1{}", "0".repeat(31)))),
        ]);
        let (hits, statements) = run(
            vec![
                vec![commit],
                vec![name_row("project", None, "deadbeef1", false)],
            ],
            "deadbeef1",
        )
        .await;

        assert_eq!(
            routes(&hits),
            [
                (
                    HitKind::Commit,
                    format!("/project/infra/task/hosts?eval={evaluation}").as_str()
                ),
                (HitKind::Project, "/project/deadbeef1"),
            ]
        );
        assert_eq!(hits[0].label, "deadbee");
        assert_eq!(statements, 2);
    }
}
