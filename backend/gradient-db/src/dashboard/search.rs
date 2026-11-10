/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::Viewer;
use gradient_types::*;
use sea_orm::{ConnectionTrait, DbErr, QueryResult, Value};

crate::sql! {
    READABLE_CACHE_IDS = concat!("SELECT c.id FROM cache c WHERE ", cache_readable!("$2")),
        params = [UserId, Bool(true)],
        tier = Bulk;

    SEARCH_NARS = concat!("SELECT c.id AS cache_id, c.name AS cache, c.display_name AS cache_display_name, \
        cp.hash, cp.package FROM cached_path cp \
        JOIN cached_path_signature s ON s.cached_path = cp.id JOIN cache c ON c.id = s.cache \
        WHERE (c.id, cp.hash) IN (SELECT * FROM unnest($2::uuid[], $3::text[])) AND ", cache_readable!("$4"), " \
        ORDER BY c.name LIMIT 10"),
        params = [UserId, CacheIds(4), CachedPathHashes(4), Bool(true)],
        tier = Bulk;

    SEARCH_COMMITS = concat!("SELECT project, project_display_name, task, task_display_name, evaluation, hash FROM ( \
        SELECT DISTINCT ON (c.hash, t.id) p.name AS project, p.display_name AS project_display_name, \
        t.name AS task, t.display_name AS task_display_name, e.id AS evaluation, \
        encode(c.hash, 'hex') AS hash, e.created_at FROM commit c JOIN evaluation e ON e.commit = c.id \
        JOIN task t ON t.id = e.task JOIN project p ON p.id = t.project \
        WHERE c.hash BETWEEN decode($2, 'hex') AND decode($3, 'hex') AND ", project_readable!("$4"), " \
        ORDER BY c.hash, t.id, e.created_at DESC) latest ORDER BY created_at DESC LIMIT 5"),
        params = [UserId, CommitPrefixLow, CommitPrefixHigh, Bool(true)],
        tier = Hot;

    SEARCH_NAMES = concat!("SELECT kind, project, project_display_name, name, display_name, starred FROM ( \
        SELECT matched.*, row_number() OVER (PARTITION BY kind ORDER BY starred DESC, match_rank, name COLLATE \"C\") AS rank FROM ( \
        SELECT hits.*, CASE WHEN lower(name) = $5 OR lower(display_name) = $5 THEN 0 \
            WHEN starts_with(lower(name), $5) OR starts_with(lower(display_name), $5) THEN 1 ELSE 2 END AS match_rank FROM ( \
            SELECT 'project' AS kind, NULL::text AS project, NULL::text AS project_display_name, p.name, p.display_name, \
                EXISTS (SELECT 1 FROM user_project_star s WHERE s.\"user\" = $1 AND s.project = p.id) AS starred \
                FROM project p WHERE (p.name ILIKE $2 OR p.display_name ILIKE $2) AND ", project_readable!("$4"), " \
            UNION ALL SELECT 'task', p.name, p.display_name, t.name, t.display_name, \
                EXISTS (SELECT 1 FROM user_task_star s WHERE s.\"user\" = $1 AND s.task = t.id) \
                FROM task t JOIN project p ON p.id = t.project \
                WHERE (t.name ILIKE $2 OR t.display_name ILIKE $2) AND ", project_readable!("($4 OR EXISTS \
                    (SELECT 1 FROM user_task_star ts WHERE ts.\"user\" = $1 AND ts.task = t.id))"), " \
            UNION ALL SELECT 'cache', NULL, NULL, c.name, c.display_name, \
                EXISTS (SELECT 1 FROM user_cache_star s WHERE s.\"user\" = $1 AND s.cache = c.id) \
                FROM cache c WHERE (c.name ILIKE $2 OR c.display_name ILIKE $2) AND ", cache_readable!("$4"), " \
        ) hits) matched) ranked WHERE rank <= $3 ORDER BY starred DESC, kind, match_rank, name COLLATE \"C\""),
        params = [UserId, Text("%a%"), Int(5), Bool(true), Text("a")],
        tier = Bulk;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NarHitRow {
    pub cache_id: CacheId,
    pub cache: String,
    pub cache_display_name: String,
    pub hash: String,
    pub package: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitHitRow {
    pub project: String,
    pub project_display_name: String,
    pub task: String,
    pub task_display_name: String,
    pub evaluation: EvaluationId,
    pub hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NameKind {
    Project,
    Task {
        project: String,
        project_display_name: String,
    },
    Cache,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameHitRow {
    pub kind: NameKind,
    pub name: String,
    pub display_name: String,
    pub starred: bool,
}

pub fn ilike_contains(text: &str) -> String {
    let escaped = text
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

fn name_kind(
    raw: &str,
    project: Option<String>,
    project_display_name: Option<String>,
) -> Result<NameKind, DbErr> {
    match (raw, project) {
        ("project", _) => Ok(NameKind::Project),
        ("task", Some(project)) => Ok(NameKind::Task {
            project_display_name: project_display_name.unwrap_or_else(|| project.clone()),
            project,
        }),
        ("task", None) => Err(DbErr::Type("task hit without a project".into())),
        ("cache", _) => Ok(NameKind::Cache),
        (other, _) => Err(DbErr::Type(format!("unknown search kind {other}"))),
    }
}

fn nar_hit_row(r: &QueryResult) -> Result<NarHitRow, DbErr> {
    Ok(NarHitRow {
        cache_id: r.try_get("", "cache_id")?,
        cache: r.try_get("", "cache")?,
        cache_display_name: r.try_get("", "cache_display_name")?,
        hash: r.try_get("", "hash")?,
        package: r.try_get("", "package")?,
    })
}

fn commit_hit_row(r: &QueryResult) -> Result<CommitHitRow, DbErr> {
    Ok(CommitHitRow {
        project: r.try_get("", "project")?,
        project_display_name: r.try_get("", "project_display_name")?,
        task: r.try_get("", "task")?,
        task_display_name: r.try_get("", "task_display_name")?,
        evaluation: EvaluationId::new(r.try_get("", "evaluation")?),
        hash: r.try_get("", "hash")?,
    })
}

pub(super) fn name_hit_row(r: &QueryResult) -> Result<NameHitRow, DbErr> {
    Ok(NameHitRow {
        kind: name_kind(
            &r.try_get::<String>("", "kind")?,
            r.try_get("", "project")?,
            r.try_get("", "project_display_name")?,
        )?,
        name: r.try_get("", "name")?,
        display_name: r.try_get("", "display_name")?,
        starred: r.try_get("", "starred")?,
    })
}

pub async fn readable_cache_ids<C: ConnectionTrait>(
    db: &C,
    viewer: Viewer,
) -> Result<Vec<CacheId>, DbErr> {
    db.query_all_raw(READABLE_CACHE_IDS.bind([viewer.user_value(), viewer.listing_value()]))
        .await?
        .iter()
        .map(|r| r.try_get("", "id"))
        .collect()
}

pub async fn search_nars<C: ConnectionTrait>(
    db: &C,
    lookups: &[(CacheId, StoreHash)],
    viewer: Viewer,
) -> Result<Vec<NarHitRow>, DbErr> {
    let (caches, hashes): (Vec<uuid::Uuid>, Vec<String>) = lookups
        .iter()
        .map(|(cache, hash)| (cache.into_inner(), hash.to_string()))
        .unzip();
    db.query_all_raw(SEARCH_NARS.bind([
        viewer.user_value(),
        Value::from(caches),
        Value::from(hashes),
        viewer.listing_value(),
    ]))
    .await?
    .iter()
    .map(nar_hit_row)
    .collect()
}

pub async fn search_commits<C: ConnectionTrait>(
    db: &C,
    low: &str,
    high: &str,
    viewer: Viewer,
) -> Result<Vec<CommitHitRow>, DbErr> {
    let stmt = SEARCH_COMMITS.bind([
        viewer.user_value(),
        low.into(),
        high.into(),
        viewer.listing_value(),
    ]);
    db.query_all_raw(stmt)
        .await?
        .iter()
        .map(commit_hit_row)
        .collect()
}

pub async fn search_names<C: ConnectionTrait>(
    db: &C,
    text: &str,
    per_kind: u64,
    viewer: Viewer,
) -> Result<Vec<NameHitRow>, DbErr> {
    let stmt = SEARCH_NAMES.bind([
        viewer.user_value(),
        ilike_contains(text).into(),
        Value::BigInt(Some(per_kind as i64)),
        viewer.listing_value(),
        text.to_lowercase().into(),
    ]);
    db.query_all_raw(stmt)
        .await?
        .iter()
        .map(name_hit_row)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ilike_wildcards_in_the_query_match_literally() {
        assert_eq!(ilike_contains(r"50%_a\b"), r"%50\%\_a\\b%");
    }

    #[test]
    fn commit_search_returns_one_row_per_commit_and_task() {
        let sql = SEARCH_COMMITS.text();
        assert!(sql.contains("DISTINCT ON (c.hash, t.id)"), "{sql}");
        assert!(
            sql.contains("ORDER BY c.hash, t.id, e.created_at DESC"),
            "{sql}"
        );
    }

    #[tokio::test]
    async fn name_search_lists_exact_then_prefix_then_other_matches() {
        let db = gradient_fixtures::database::TestDatabase::create().await;
        let conn = db.connection();
        let user = crate::rows::insert::<EUser, _>(&conn, gradient_fixtures::rows::user())
            .await
            .expect("the user is written");
        for name in ["my-tools", "tools-extra", "atools", "Tools"] {
            crate::rows::insert::<EProject, _>(
                &conn,
                MProject {
                    public: true,
                    ..gradient_fixtures::rows::project_with_id(ProjectId::now_v7(), name)
                },
            )
            .await
            .expect("the project is written");
        }

        let hits = search_names(
            &conn,
            "tools",
            5,
            Viewer {
                user: user.id,
                public_listing: true,
            },
        )
        .await
        .expect("the search runs");

        let names: Vec<&str> = hits.iter().map(|hit| hit.name.as_str()).collect();
        assert_eq!(names, ["Tools", "tools-extra", "atools", "my-tools"]);
        db.finish().await;
    }
}
