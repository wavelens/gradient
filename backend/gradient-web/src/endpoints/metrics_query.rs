/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Discoverable, access-controlled query surface over `metric_rollup`.
//!
//! `GET /metrics/catalog` lists the available metrics; `GET /metrics/query`
//! returns time-series points masked to the caller's scope: superusers see all
//! orgs, members see their orgs plus public orgs, anonymous callers see public
//! orgs only.

use crate::authorization::MaybeUser;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::metrics_scope::MetricsScope;
use axum::extract::{Query, State};
use axum::{Extension, Json};
use gradient_types::*;
use gradient_core::ServerState;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement, Value};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Serialize)]
pub struct MetricMeta {
    pub key: &'static str,
    pub kind: &'static str,
    pub unit: &'static str,
    pub dimensions: &'static [&'static str],
}

/// The advertised metric surface. Keys present here are accepted by `query`;
/// keys whose aggregator has not landed yet simply return no points.
const CATALOG: &[MetricMeta] = &[
    MetricMeta { key: "builds.created", kind: "counter", unit: "builds", dimensions: &["org"] },
    MetricMeta { key: "builds.dispatched", kind: "counter", unit: "builds", dimensions: &["org"] },
    MetricMeta { key: "builds.completed", kind: "counter", unit: "builds", dimensions: &["org"] },
    MetricMeta { key: "builds.substituted", kind: "counter", unit: "builds", dimensions: &["org"] },
    MetricMeta { key: "builds.failed", kind: "counter", unit: "builds", dimensions: &["org"] },
    MetricMeta { key: "evals.completed", kind: "counter", unit: "evals", dimensions: &["org"] },
    MetricMeta { key: "evals.failed", kind: "counter", unit: "evals", dimensions: &["org"] },
    MetricMeta { key: "builds.duration_ms", kind: "histogram", unit: "ms", dimensions: &["org"] },
    MetricMeta { key: "dispatch.wait_ms", kind: "histogram", unit: "ms", dimensions: &["org"] },
    MetricMeta { key: "deps.wait_ms", kind: "histogram", unit: "ms", dimensions: &["org"] },
];

pub async fn get_metrics_catalog() -> WebResult<Json<BaseResponse<Vec<&'static MetricMeta>>>> {
    Ok(ok_json(CATALOG.iter().collect()))
}

#[derive(Deserialize)]
pub struct MetricQueryParams {
    pub metric: String,
    pub granularity: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub org: Option<Uuid>,
}

#[derive(Serialize)]
pub struct MetricPoint {
    pub bucket_start: String,
    pub count: i64,
    pub sum: f64,
    pub min: f64,
    pub max: f64,
    pub avg: f64,
}

fn granularity_code(g: Option<&str>) -> i16 {
    match g {
        Some("minute") => 0,
        Some("hour") => 1,
        Some("week") => 3,
        Some("day") | None => 2,
        _ => 2,
    }
}

pub async fn get_metrics_query(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Query(params): Query<MetricQueryParams>,
) -> WebResult<Json<BaseResponse<Vec<MetricPoint>>>> {
    if !CATALOG.iter().any(|m| m.key == params.metric) {
        return Err(WebError::not_found("Metric"));
    }

    let gran = granularity_code(params.granularity.as_deref());
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;

    // org filter: an explicit org must be inside the caller's scope.
    let org_filter: Option<Vec<String>> = match (&scope, params.org) {
        (MetricsScope::All, Some(o)) => Some(vec![o.to_string()]),
        (MetricsScope::All, None) => None,
        (MetricsScope::Orgs(orgs), Some(o)) => {
            let o = o.to_string();
            if !orgs.contains(&o) {
                return Err(WebError::not_found("Metric"));
            }

            Some(vec![o])
        }
        (MetricsScope::Orgs(orgs), None) => {
            if orgs.is_empty() {
                return Ok(ok_json(vec![]));
            }

            Some(orgs.clone())
        }
    };

    let mut sql = String::from(
        "SELECT bucket_start, sum(count)::bigint AS c, sum(sum) AS s, \
                min(min) AS mn, max(max) AS mx \
         FROM metric_rollup WHERE metric = $1 AND granularity = $2",
    );

    let mut values: Vec<Value> = vec![Value::from(params.metric.clone()), Value::from(gran)];
    if let Some(orgs) = &org_filter {
        // DB-sourced UUID strings, safe to inline as a quoted IN list.
        let list = orgs
            .iter()
            .map(|o| format!("'{o}'"))
            .collect::<Vec<_>>()
            .join(",");

        sql.push_str(&format!(" AND (scope->>'org') IN ({list})"));
    }

    if let Some(from) = parse_ts(params.from.as_deref()) {
        values.push(Value::from(from));
        sql.push_str(&format!(" AND bucket_start >= ${}", values.len()));
    }

    if let Some(to) = parse_ts(params.to.as_deref()) {
        values.push(Value::from(to));
        sql.push_str(&format!(" AND bucket_start <= ${}", values.len()));
    }

    sql.push_str(" GROUP BY bucket_start ORDER BY bucket_start");

    let rows = state
        .web_db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            values,
        ))
        .await?;

    let points = rows
        .into_iter()
        .map(|r| {
            let count: i64 = r.try_get("", "c").unwrap_or(0);
            let sum: f64 = r.try_get("", "s").unwrap_or(0.0);
            let bucket: chrono::NaiveDateTime = r.try_get("", "bucket_start").unwrap_or_default();
            MetricPoint {
                bucket_start: bucket.and_utc().to_rfc3339(),
                count,
                sum,
                min: r.try_get("", "mn").unwrap_or(0.0),
                max: r.try_get("", "mx").unwrap_or(0.0),
                avg: if count > 0 { sum / count as f64 } else { 0.0 },
            }
        })
        .collect();

    Ok(ok_json(points))
}

fn parse_ts(s: Option<&str>) -> Option<chrono::NaiveDateTime> {
    let s = s?;
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.naive_utc());
    }

    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").ok()
}
