/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::MaybeUser;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::metrics_scope::MetricsScope;
use crate::resource::Resource;
use axum::extract::{Query, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_entity::metric_rollup::RollupGranularity;
use gradient_types::*;
use gradient_util::telemetry::metric;
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

const CATALOG: &[MetricMeta] = &[
    MetricMeta {
        key: metric::BUILDS_CREATED,
        kind: "counter",
        unit: "builds",
        dimensions: &["project"],
    },
    MetricMeta {
        key: metric::BUILDS_DISPATCHED,
        kind: "counter",
        unit: "builds",
        dimensions: &["project"],
    },
    MetricMeta {
        key: metric::BUILDS_COMPLETED,
        kind: "counter",
        unit: "builds",
        dimensions: &["project"],
    },
    MetricMeta {
        key: metric::BUILDS_SUBSTITUTED,
        kind: "counter",
        unit: "builds",
        dimensions: &["project"],
    },
    MetricMeta {
        key: metric::BUILDS_FAILED,
        kind: "counter",
        unit: "builds",
        dimensions: &["project"],
    },
    MetricMeta {
        key: metric::EVALS_COMPLETED,
        kind: "counter",
        unit: "evals",
        dimensions: &["project"],
    },
    MetricMeta {
        key: metric::EVALS_FAILED,
        kind: "counter",
        unit: "evals",
        dimensions: &["project"],
    },
    MetricMeta {
        key: metric::BUILDS_DURATION_MS,
        kind: "histogram",
        unit: "ms",
        dimensions: &["project"],
    },
    MetricMeta {
        key: metric::DISPATCH_WAIT_MS,
        kind: "histogram",
        unit: "ms",
        dimensions: &["project"],
    },
    MetricMeta {
        key: metric::DEPS_WAIT_MS,
        kind: "histogram",
        unit: "ms",
        dimensions: &["project"],
    },
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
    pub project: Option<Uuid>,
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

impl From<gradient_db::metrics::series::BucketStats> for MetricPoint {
    fn from(stats: gradient_db::metrics::series::BucketStats) -> Self {
        Self {
            bucket_start: stats.bucket_start.and_utc().to_rfc3339(),
            count: stats.count,
            sum: stats.sum,
            min: stats.min,
            max: stats.max,
            avg: if stats.count > 0 {
                stats.sum / stats.count as f64
            } else {
                0.0
            },
        }
    }
}

pub async fn get_metrics_query(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Query(params): Query<MetricQueryParams>,
) -> WebResult<Json<BaseResponse<Vec<MetricPoint>>>> {
    if !CATALOG.iter().any(|m| m.key == params.metric) {
        return Err(WebError::not_found(Resource::Metric));
    }

    let gran = RollupGranularity::from_query_param(params.granularity.as_deref());
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;

    let project_filter: Option<Vec<String>> = match (&scope, params.project) {
        (MetricsScope::All, Some(o)) => Some(vec![o.to_string()]),
        (MetricsScope::All, None) => None,
        (MetricsScope::Projects(projects), Some(o)) => {
            let o = o.to_string();
            if !projects.contains(&o) {
                return Err(WebError::not_found(Resource::Metric));
            }

            Some(vec![o])
        }
        (MetricsScope::Projects(projects), None) => {
            if projects.is_empty() {
                return Ok(ok_json(vec![]));
            }

            Some(projects.clone())
        }
    };

    let project_list = project_filter.as_ref().map(|projects| {
        projects
            .iter()
            .map(|o| format!("'{o}'"))
            .collect::<Vec<_>>()
            .join(",")
    });
    let from = parse_ts(params.from.as_deref());
    let to = parse_ts(params.to.as_deref());

    let rows = gradient_db::metrics::series::of_projects(
        &state.web_db,
        &params.metric,
        gran,
        project_list.as_deref(),
        from,
        to,
    )
    .await?;

    let points = rows.into_iter().map(MetricPoint::from).collect();

    Ok(ok_json(points))
}

fn parse_ts(s: Option<&str>) -> Option<chrono::NaiveDateTime> {
    let s = s?;
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.naive_utc());
    }

    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").ok()
}
