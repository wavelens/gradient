/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::stars::user_value;
use gradient_types::*;
use sea_orm::{ConnectionTrait, DbErr, QueryResult, Value};

crate::sql! {
    SEARCH_NARS = concat!("SELECT c.name AS cache, cp.hash, cp.package FROM cached_path cp \
        JOIN cached_path_signature s ON s.cached_path = cp.id JOIN cache c ON c.id = s.cache \
        WHERE cp.hash = $2 AND ", cache_readable!(), " ORDER BY c.name LIMIT 10"),
        params = [UserId, CachedPathHash],
        tier = Bulk;

    SEARCH_COMMITS = concat!("SELECT p.name AS project, t.name AS task, e.id AS evaluation, \
        encode(c.hash, 'hex') AS hash FROM commit c JOIN evaluation e ON e.commit = c.id \
        JOIN task t ON t.id = e.task JOIN project p ON p.id = t.project \
        WHERE c.hash BETWEEN decode($2, 'hex') AND decode($3, 'hex') AND ", project_readable!(), " \
        ORDER BY e.created_at DESC LIMIT 5"),
        params = [UserId, CommitPrefixLow, CommitPrefixHigh],
        tier = Hot;

    SEARCH_NAMES = concat!("SELECT kind, project, name, display_name, starred FROM ( \
        SELECT hits.*, row_number() OVER (PARTITION BY kind ORDER BY starred DESC, name) AS rank FROM ( \
            SELECT 'project' AS kind, NULL::text AS project, p.name, p.display_name, \
                EXISTS (SELECT 1 FROM user_project_star s WHERE s.\"user\" = $1 AND s.project = p.id) AS starred \
                FROM project p WHERE (p.name ILIKE $2 OR p.display_name ILIKE $2) AND ", project_readable!(), " \
            UNION ALL SELECT 'task', p.name, t.name, t.display_name, \
                EXISTS (SELECT 1 FROM user_task_star s WHERE s.\"user\" = $1 AND s.task = t.id) \
                FROM task t JOIN project p ON p.id = t.project \
                WHERE (t.name ILIKE $2 OR t.display_name ILIKE $2) AND ", project_readable!(), " \
            UNION ALL SELECT 'cache', NULL, c.name, c.display_name, \
                EXISTS (SELECT 1 FROM user_cache_star s WHERE s.\"user\" = $1 AND s.cache = c.id) \
                FROM cache c WHERE (c.name ILIKE $2 OR c.display_name ILIKE $2) AND ", cache_readable!(), " \
        ) hits) ranked WHERE rank <= $3 ORDER BY starred DESC, kind, name"),
        params = [UserId, Text("%a%"), Int(5)],
        tier = Bulk;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NarHitRow {
    pub cache: String,
    pub hash: String,
    pub package: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitHitRow {
    pub project: String,
    pub task: String,
    pub evaluation: EvaluationId,
    pub hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NameKind {
    Project,
    Task { project: String },
    Cache,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameHitRow {
    pub kind: NameKind,
    pub name: String,
    pub display_name: String,
    pub starred: bool,
}

fn ilike_contains(text: &str) -> String {
    let escaped = text
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

fn name_kind(raw: &str, project: Option<String>) -> Result<NameKind, DbErr> {
    match (raw, project) {
        ("project", _) => Ok(NameKind::Project),
        ("task", Some(project)) => Ok(NameKind::Task { project }),
        ("task", None) => Err(DbErr::Type("task hit without a project".into())),
        ("cache", _) => Ok(NameKind::Cache),
        (other, _) => Err(DbErr::Type(format!("unknown search kind {other}"))),
    }
}

fn nar_hit_row(r: &QueryResult) -> Result<NarHitRow, DbErr> {
    Ok(NarHitRow {
        cache: r.try_get("", "cache")?,
        hash: r.try_get("", "hash")?,
        package: r.try_get("", "package")?,
    })
}

fn commit_hit_row(r: &QueryResult) -> Result<CommitHitRow, DbErr> {
    Ok(CommitHitRow {
        project: r.try_get("", "project")?,
        task: r.try_get("", "task")?,
        evaluation: EvaluationId::new(r.try_get("", "evaluation")?),
        hash: r.try_get("", "hash")?,
    })
}

pub(super) fn name_hit_row(r: &QueryResult) -> Result<NameHitRow, DbErr> {
    Ok(NameHitRow {
        kind: name_kind(&r.try_get::<String>("", "kind")?, r.try_get("", "project")?)?,
        name: r.try_get("", "name")?,
        display_name: r.try_get("", "display_name")?,
        starred: r.try_get("", "starred")?,
    })
}

pub async fn search_nars<C: ConnectionTrait>(
    db: &C,
    hash: &str,
    user: UserId,
) -> Result<Vec<NarHitRow>, DbErr> {
    db.query_all_raw(SEARCH_NARS.bind([user_value(user), hash.into()]))
        .await?
        .iter()
        .map(nar_hit_row)
        .collect()
}

pub async fn search_commits<C: ConnectionTrait>(
    db: &C,
    low: &str,
    high: &str,
    user: UserId,
) -> Result<Vec<CommitHitRow>, DbErr> {
    let stmt = SEARCH_COMMITS.bind([user_value(user), low.into(), high.into()]);
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
    user: UserId,
) -> Result<Vec<NameHitRow>, DbErr> {
    let stmt = SEARCH_NAMES.bind([
        user_value(user),
        ilike_contains(text).into(),
        Value::BigInt(Some(per_kind as i64)),
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
}
