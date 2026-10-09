/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::disallowed_methods,
    reason = "the samplers draw parameter values and are not statements the gate measures"
)]

use std::collections::HashMap;

use anyhow::Result;
use gradient_db::sql::Param;
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, Value};
use uuid::Uuid;

/// Every draw is ordered because heap order is moving with each update. An unordered draw measured
/// evaluations 100x apart in size between runs. The single evaluation is the one naming the most
/// shared builds, the worst case a statement scoped to one must fit.
pub fn draw_sql(param: &Param) -> Option<&'static str> {
    Some(match param {
        Param::DerivationId => "SELECT id AS v FROM derivation ORDER BY id LIMIT 1",
        Param::DerivationIds(_) => {
            "SELECT array_agg(id) AS v FROM (SELECT id FROM derivation ORDER BY id LIMIT $1) s"
        }
        Param::OrphanDerivationIds(_) => {
            "SELECT array_agg(id) AS v FROM (\
                 SELECT d.id FROM derivation d \
                 WHERE NOT EXISTS (SELECT 1 FROM build_job bj WHERE bj.derivation = d.id) \
                   AND NOT EXISTS (SELECT 1 FROM entry_point ep WHERE ep.derivation = d.id) \
                 ORDER BY d.id LIMIT $1) s"
        }
        Param::DerivationHash => "SELECT hash AS v FROM derivation ORDER BY id LIMIT 1",
        Param::DerivationHashes(_) => {
            "SELECT array_agg(hash) AS v FROM (SELECT hash FROM derivation ORDER BY id LIMIT $1) s"
        }
        Param::CachedPathId => "SELECT id AS v FROM cached_path ORDER BY id LIMIT 1",
        Param::CachedPathHash => "SELECT hash AS v FROM cached_path ORDER BY id LIMIT 1",
        Param::CachedPathHashes(_) => {
            "SELECT array_agg(hash) AS v FROM (SELECT hash FROM cached_path ORDER BY id LIMIT $1) s"
        }
        Param::SharedBuildId => "SELECT id AS v FROM derivation_build ORDER BY id LIMIT 1",
        Param::SharedBuildIds(_) => {
            "SELECT array_agg(id) AS v FROM (SELECT id FROM derivation_build ORDER BY id LIMIT $1) s"
        }
        Param::EvaluationId => {
            "SELECT evaluation AS v FROM build_job \
             GROUP BY evaluation ORDER BY count(*) DESC, evaluation LIMIT 1"
        }
        Param::EvaluationIds(_) => {
            "SELECT array_agg(id) AS v FROM (SELECT id FROM evaluation ORDER BY id LIMIT $1) s"
        }
        Param::EntryPointId => "SELECT id AS v FROM entry_point ORDER BY id LIMIT 1",
        Param::EntryPointIds(_) => {
            "SELECT array_agg(id) AS v FROM (SELECT id FROM entry_point ORDER BY id LIMIT $1) s"
        }
        Param::ProjectId => "SELECT id AS v FROM project ORDER BY id LIMIT 1",
        Param::UserId => r#"SELECT id AS v FROM "user" ORDER BY id LIMIT 1"#,
        Param::CacheId => "SELECT id AS v FROM cache ORDER BY id LIMIT 1",
        Param::CacheIds(_) => {
            "SELECT array_agg(id) AS v FROM (SELECT id FROM cache ORDER BY id LIMIT $1) s"
        }
        Param::TaskId => "SELECT id AS v FROM task ORDER BY id LIMIT 1",
        Param::TaskActionId => "SELECT id AS v FROM task_action ORDER BY id LIMIT 1",
        Param::IntegrationId => "SELECT id AS v FROM integration ORDER BY id LIMIT 1",
        Param::CommitPrefixLow => {
            "SELECT rpad(left(encode(c.hash, 'hex'), 7), 40, '0') AS v FROM commit c \
             WHERE EXISTS (SELECT 1 FROM evaluation e WHERE e.commit = c.id) ORDER BY c.id LIMIT 1"
        }
        Param::CommitPrefixHigh => {
            "SELECT rpad(left(encode(c.hash, 'hex'), 7), 40, 'f') AS v FROM commit c \
             WHERE EXISTS (SELECT 1 FROM evaluation e WHERE e.commit = c.id) ORDER BY c.id LIMIT 1"
        }
        Param::NewUuid
        | Param::NewUuids(_)
        | Param::Text(_)
        | Param::Int(_)
        | Param::Bool(_)
        | Param::Texts(..)
        | Param::Ints(..)
        | Param::Bools(..)
        | Param::Now => return None,
    })
}

enum Shape {
    Uuid,
    Uuids(usize),
    Text,
    Texts(usize),
}

fn shape(param: &Param) -> Option<Shape> {
    Some(match param {
        Param::DerivationId
        | Param::CachedPathId
        | Param::SharedBuildId
        | Param::EvaluationId
        | Param::EntryPointId
        | Param::ProjectId
        | Param::UserId
        | Param::CacheId
        | Param::TaskId
        | Param::TaskActionId
        | Param::IntegrationId => Shape::Uuid,
        Param::DerivationIds(n)
        | Param::OrphanDerivationIds(n)
        | Param::SharedBuildIds(n)
        | Param::EvaluationIds(n)
        | Param::EntryPointIds(n)
        | Param::CacheIds(n) => Shape::Uuids(*n),
        Param::DerivationHash
        | Param::CachedPathHash
        | Param::CommitPrefixLow
        | Param::CommitPrefixHigh => Shape::Text,
        Param::DerivationHashes(n) | Param::CachedPathHashes(n) => Shape::Texts(*n),
        Param::NewUuid
        | Param::NewUuids(_)
        | Param::Text(_)
        | Param::Int(_)
        | Param::Bool(_)
        | Param::Texts(..)
        | Param::Ints(..)
        | Param::Bools(..)
        | Param::Now => return None,
    })
}

#[derive(Default)]
pub struct Sampler {
    drawn: HashMap<String, Option<Value>>,
}

impl Sampler {
    pub fn pin(&mut self, param: &Param, value: Value) {
        self.drawn.insert(format!("{param:?}"), Some(value));
    }

    pub async fn value(&mut self, db: &DatabaseConnection, param: &Param) -> Result<Option<Value>> {
        if let Some(literal) = literal(param) {
            return Ok(Some(literal));
        }

        let key = format!("{param:?}");
        if let Some(cached) = self.drawn.get(&key) {
            return Ok(cached.clone());
        }

        let drawn = self.draw(db, param).await?;
        self.drawn.insert(key, drawn.clone());

        Ok(drawn)
    }

    async fn draw(&self, db: &DatabaseConnection, param: &Param) -> Result<Option<Value>> {
        let (Some(sql), Some(shape)) = (draw_sql(param), shape(param)) else {
            return Ok(None);
        };

        let stmt = match shape {
            Shape::Uuids(width) | Shape::Texts(width) => Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                [Value::from(width as i64)],
            ),
            Shape::Uuid | Shape::Text => Statement::from_string(DatabaseBackend::Postgres, sql),
        };

        let Some(row) = db.query_one_raw(stmt).await? else {
            return Ok(None);
        };

        Ok(match shape {
            Shape::Uuid => row.try_get::<Option<Uuid>>("", "v")?.map(Value::from),
            Shape::Text => row.try_get::<Option<String>>("", "v")?.map(Value::from),
            Shape::Uuids(_) => row
                .try_get::<Option<Vec<Uuid>>>("", "v")?
                .filter(|values| !values.is_empty())
                .map(Value::from),
            Shape::Texts(_) => row
                .try_get::<Option<Vec<String>>>("", "v")?
                .filter(|values| !values.is_empty())
                .map(Value::from),
        })
    }
}

fn literal(param: &Param) -> Option<Value> {
    Some(match param {
        Param::Text(text) => Value::from(*text),
        Param::Int(n) => Value::from(*n),
        Param::Bool(b) => Value::from(*b),
        Param::NewUuid => Value::from(Uuid::now_v7()),
        Param::NewUuids(width) => {
            Value::from((0..*width).map(|_| Uuid::now_v7()).collect::<Vec<Uuid>>())
        }
        Param::Texts(text, width) => Value::from(vec![(*text).to_string(); *width]),
        Param::Ints(n, width) => Value::from(vec![*n; *width]),
        Param::Bools(b, width) => Value::from(vec![*b; *width]),
        Param::Now => Value::from(sea_orm::prelude::DateTimeUtc::from(
            std::time::SystemTime::now(),
        )),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use gradient_db::sql::Param;

    use super::draw_sql;

    #[test]
    fn every_drawn_kind_has_a_sampler() {
        for param in [
            Param::DerivationId,
            Param::DerivationIds(4),
            Param::OrphanDerivationIds(4),
            Param::DerivationHash,
            Param::DerivationHashes(4),
            Param::CachedPathId,
            Param::CachedPathHash,
            Param::CachedPathHashes(4),
            Param::SharedBuildId,
            Param::SharedBuildIds(4),
            Param::EvaluationId,
            Param::EvaluationIds(4),
            Param::EntryPointId,
            Param::EntryPointIds(4),
            Param::ProjectId,
            Param::UserId,
            Param::CacheId,
            Param::TaskId,
            Param::TaskActionId,
            Param::IntegrationId,
            Param::CommitPrefixLow,
            Param::CommitPrefixHigh,
        ] {
            let sql = draw_sql(&param).unwrap_or_else(|| panic!("{param:?} has no sampler"));
            assert!(
                sql.contains("ORDER BY"),
                "{param:?} draws in heap order: {sql}"
            );
        }
    }

    #[test]
    fn literal_kinds_need_no_query() {
        for param in [
            Param::Text("x"),
            Param::Int(1),
            Param::Bool(true),
            Param::NewUuid,
            Param::NewUuids(4),
            Param::Texts("x", 4),
            Param::Ints(1, 4),
            Param::Bools(true, 4),
            Param::Now,
        ] {
            assert!(draw_sql(&param).is_none(), "{param:?} must not query");
        }
    }

    #[test]
    fn an_array_sampler_asks_for_the_declared_width() {
        assert!(
            draw_sql(&Param::DerivationIds(64))
                .expect("sampler")
                .contains("$1")
        );
    }
}
