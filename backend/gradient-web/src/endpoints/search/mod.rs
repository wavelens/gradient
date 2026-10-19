/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod classify;

use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use axum::extract::{Query, State};
use axum::{Extension, Json};
use classify::{Lookups, classify};
use gradient_core::ServerState;
use gradient_db::dashboard::{
    CommitHitRow, NameHitRow, NameKind, NarHitRow, readable_cache_ids, search_commits,
    search_names, search_nars, starred,
};
use gradient_types::*;
use sea_orm::{ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SearchQuery {
    #[serde(default)]
    pub q: String,
    pub limit: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HitKind {
    Project,
    Task,
    Cache,
    Nar,
    Commit,
}

#[derive(Debug, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct SearchHit {
    pub kind: HitKind,
    pub label: String,
    pub sublabel: String,
    pub route: String,
    pub starred: bool,
}

impl From<NameHitRow> for SearchHit {
    fn from(n: NameHitRow) -> Self {
        let mut hit = match n.kind {
            NameKind::Project => project_hit(n.name, n.starred),
            NameKind::Task { project } => task_hit(project, n.name, n.starred),
            NameKind::Cache => cache_hit(n.name, n.starred),
        };
        hit.label = n.display_name;
        hit
    }
}

impl From<CommitHitRow> for SearchHit {
    fn from(c: CommitHitRow) -> Self {
        Self {
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
}

fn nar_hit(n: NarHitRow, typed: &StoreHash) -> SearchHit {
    SearchHit {
        kind: HitKind::Nar,
        route: format!("/caches/{}/nars?hash={typed}", n.cache),
        label: n.package,
        sublabel: n.cache,
        starred: false,
    }
}

fn nar_lookups(
    hash: &StoreHash,
    caches: &[CacheId],
    mapping: &Arc<dyn StorePathMapping>,
) -> Vec<(CacheId, StoreHash)> {
    caches
        .iter()
        .map(|&cache| {
            let stored = mapping.inbound(MappingContext::Cache(cache), hash.clone());
            (cache, stored)
        })
        .collect()
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

pub async fn search<C: ConnectionTrait>(
    db: &C,
    mapping: &Arc<dyn StorePathMapping>,
    user: UserId,
    lookups: Lookups,
    per_kind: u64,
) -> Result<Vec<SearchHit>, DbErr> {
    let Some(text) = lookups.text else {
        return Ok(starred(db, user)
            .await?
            .into_iter()
            .map(SearchHit::from)
            .collect());
    };
    let mut hits = Vec::new();
    if let Some(hash) = &lookups.nar {
        let caches = readable_cache_ids(db, user).await?;
        let nars = search_nars(db, &nar_lookups(hash, &caches, mapping), user).await?;
        hits.extend(nars.into_iter().map(|n| nar_hit(n, hash)));
    }
    if let Some(range) = &lookups.commit {
        let commits = search_commits(db, &range.low, &range.high, user).await?;
        hits.extend(commits.into_iter().map(SearchHit::from));
    }
    let names = search_names(db, &text, per_kind, user).await?;
    hits.extend(names.into_iter().map(SearchHit::from));
    Ok(hits)
}

#[utoipa::path(
    get,
    path = "/search",
    operation_id = "getSearch",
    tag = "dashboard",
    summary = "Typed search for the command palette",
    description = "Classifies `q` and performs every lookup it could mean. The name lookup always takes place, and a project named like a commit hash is still found.\n- Store path or bare 32-char nix32 hash: NARs of that hash in readable caches (`nar`, at most 10).\n- 7 to 40 hex characters: evaluations of commits with that prefix in readable projects, newest first, PR-triggered ones included (`commit`, at most 5).\n- Any text: projects, tasks and caches whose name or display name contains it, at most `limit` per kind, starred first.\nAn empty `q` returns the user's starred projects, tasks and caches, labelled by display name like name hits.\nVisibility matches the rest of the API: a project is readable when public or the user is a member; a cache when public, created by, shared with or subscribed to by a project of the user. There is no superuser bypass, so every hit opens a readable page.\nResults are ordered NAR hits, commit hits, then name hits (starred first, then kind, then name).",
    params(SearchQuery),
    responses((status = OK, description = "Search hits with the frontend route each one navigates to", body = Vec<SearchHit>), WebError)
)]
pub async fn get_search(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Query(q): Query<SearchQuery>,
) -> WebResult<Json<BaseResponse<Vec<SearchHit>>>> {
    let hits = search(
        &state.web_db,
        &state.extensions.store_path_mapping,
        user.id,
        classify(&q.q),
        per_kind(q.limit),
    )
    .await?;
    Ok(ok_json(hits))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::{EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH, RotatedHashMapping};
    use gradient_fixtures::mock::mock_db;
    use sea_orm::Value;
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
        let db = mock_db().append_query_results(results).into_connection();
        let mapping: Arc<dyn StorePathMapping> = Arc::new(IdentityMapping);
        let hits = search(&db, &mapping, UserId::now_v7(), classify(q), 5)
            .await
            .unwrap();
        (hits, db.into_transaction_log().len())
    }

    #[test]
    fn a_nar_query_is_mapped_once_per_readable_cache() {
        let (c1, c2) = (CacheId::now_v7(), CacheId::now_v7());
        let hash = StoreHash::parse(EXAMPLE_ORIGINAL_HASH).unwrap();
        let stored = StoreHash::parse(EXAMPLE_STORED_HASH).unwrap();
        let mapping: Arc<dyn StorePathMapping> = Arc::new(RotatedHashMapping);
        assert_eq!(
            nar_lookups(&hash, &[c1, c2], &mapping),
            vec![(c1, stored.clone()), (c2, stored)]
        );
    }

    #[test]
    fn limit_is_clamped() {
        assert_eq!(per_kind(None), 5);
        assert_eq!(per_kind(Some(0)), 1);
        assert_eq!(per_kind(Some(999)), 20);
    }

    #[tokio::test]
    async fn an_empty_query_lists_starred_items_by_display_name() {
        let (hits, statements) = run(
            vec![vec![
                name_row("cache", None, "main", true),
                name_row("project", None, "infra", true),
                name_row("task", Some("infra"), "hosts", true),
            ]],
            "  ",
        )
        .await;

        assert_eq!(
            routes(&hits),
            [
                (HitKind::Cache, "/caches/main"),
                (HitKind::Project, "/project/infra"),
                (HitKind::Task, "/project/infra/task/hosts"),
            ]
        );
        let labels: Vec<&str> = hits.iter().map(|h| h.label.as_str()).collect();
        assert_eq!(labels, ["MAIN", "INFRA", "HOSTS"]);
        assert!(hits.iter().all(|h| h.starred));
        assert_eq!(statements, 1);
    }

    #[tokio::test]
    async fn a_task_hit_without_a_project_is_an_error() {
        let db = mock_db()
            .append_query_results([vec![name_row("task", None, "hosts", false)]])
            .into_connection();
        let mapping: Arc<dyn StorePathMapping> = Arc::new(IdentityMapping);
        let result = search(&db, &mapping, UserId::now_v7(), classify("hosts"), 5).await;
        assert!(result.is_err());
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
        let cache = CacheId::now_v7();
        let readable = BTreeMap::from([("id", Value::from(cache.into_inner()))]);
        let nar = BTreeMap::from([
            ("cache_id", Value::from(cache.into_inner())),
            ("cache", Value::from("main")),
            ("hash", Value::from(HASH)),
            ("package", Value::from("hello-2.12.1")),
        ]);
        let (hits, statements) = run(vec![vec![readable], vec![nar], vec![]], HASH).await;

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, HitKind::Nar);
        assert_eq!(hits[0].route, format!("/caches/main/nars?hash={HASH}"));
        assert_eq!(hits[0].label, "hello-2.12.1");
        assert_eq!(statements, 3);
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
