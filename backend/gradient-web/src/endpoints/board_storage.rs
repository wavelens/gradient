/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::endpoints::board_metrics::WindowParams;
use crate::error::{WebResult, require_superuser};
use crate::helpers::ok_json;
use axum::extract::{Query, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_entity::metric_rollup::RollupGranularity;
use gradient_types::*;
use gradient_util::telemetry::metric;
use sea_orm::ConnectionTrait;
use serde::Serialize;
use std::sync::Arc;

#[derive(Serialize)]
pub struct LabelledPoint {
    pub bucket_start: String,
    pub count: i64,
    pub avg: f64,
    pub max: f64,
}

#[derive(Serialize)]
pub struct LabelledSeries {
    pub label: String,
    pub points: Vec<LabelledPoint>,
}

#[derive(Serialize)]
pub struct BoardStorage {
    pub granularity: &'static str,
    pub op_latency: Vec<LabelledSeries>,
    pub op_errors: Vec<LabelledSeries>,
    pub lane_fill: Vec<LabelledSeries>,
    pub send_stalls: Vec<LabelledSeries>,
    pub serve_queue: Vec<LabelledSeries>,
    pub serve_failures: Vec<LabelledSeries>,
}

pub async fn get_board_storage(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Query(params): Query<WindowParams>,
) -> WebResult<Json<BaseResponse<BoardStorage>>> {
    require_superuser(&user)?;
    let window = params.window_hours.unwrap_or(6).clamp(1, 24 * 90);
    let granularity = granularity_for(window);
    let series = SeriesReader {
        db: &state.web_db,
        granularity,
        window_hours: window,
    };

    let mut lane_fill = series
        .relabelled(metric::PROTO_BULK_LANE_FILL, "bulk")
        .await?;
    lane_fill.extend(
        series
            .relabelled(metric::PROTO_CONTROL_LANE_FILL, "control")
            .await?,
    );

    let mut serve_queue = series
        .relabelled(metric::NAR_SERVES_WAITING, "waiting")
        .await?;
    serve_queue.extend(
        series
            .relabelled(metric::NAR_SERVES_ACTIVE, "active")
            .await?,
    );

    Ok(ok_json(BoardStorage {
        granularity: granularity.trunc_unit(),
        op_latency: series.labelled(metric::STORAGE_OP_MS).await?,
        op_errors: series.labelled(metric::STORAGE_OP_ERRORS).await?,
        lane_fill,
        send_stalls: series.labelled(metric::PROTO_SEND_STALLS).await?,
        serve_queue,
        serve_failures: series.labelled(metric::NAR_SERVE_FAILURES).await?,
    }))
}

fn granularity_for(window_hours: i64) -> RollupGranularity {
    match window_hours {
        ..=6 => RollupGranularity::Minute,
        7..=168 => RollupGranularity::Hour,
        _ => RollupGranularity::Day,
    }
}

struct SeriesReader<'a, C> {
    db: &'a C,
    granularity: RollupGranularity,
    window_hours: i64,
}

impl<C: ConnectionTrait> SeriesReader<'_, C> {
    async fn labelled(&self, metric: &str) -> WebResult<Vec<LabelledSeries>> {
        let rows = gradient_db::metrics::series::labelled(
            self.db,
            metric,
            self.granularity,
            self.window_hours,
        )
        .await?;

        Ok(group_rows(
            rows.into_iter()
                .map(|r| {
                    (
                        r.bucket_start.and_utc().to_rfc3339(),
                        r.label,
                        r.count,
                        r.sum,
                        r.max,
                    )
                })
                .collect(),
        ))
    }

    async fn relabelled(&self, metric: &str, label: &str) -> WebResult<Vec<LabelledSeries>> {
        let mut series = self.labelled(metric).await?;
        for one in &mut series {
            one.label = label.to_owned();
        }

        Ok(series)
    }
}

fn group_rows(rows: Vec<(String, String, i64, f64, f64)>) -> Vec<LabelledSeries> {
    let mut out: Vec<LabelledSeries> = Vec::new();
    for (bucket_start, label, count, sum, max) in rows {
        let point = LabelledPoint {
            bucket_start,
            count,
            avg: if count > 0 { sum / count as f64 } else { 0.0 },
            max,
        };

        match out.last_mut() {
            Some(last) if last.label == label => last.points.push(point),
            _ => out.push(LabelledSeries {
                label,
                points: vec![point],
            }),
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_windows_read_minutes() {
        assert_eq!(granularity_for(1), RollupGranularity::Minute);
        assert_eq!(granularity_for(6), RollupGranularity::Minute);
        assert_eq!(granularity_for(7), RollupGranularity::Hour);
        assert_eq!(granularity_for(168), RollupGranularity::Hour);
        assert_eq!(granularity_for(169), RollupGranularity::Day);
    }

    #[test]
    fn rows_become_one_series_per_label() {
        let rows = vec![
            (
                "2026-09-29T15:00:00+00:00".to_owned(),
                "get".to_owned(),
                2,
                10.0,
                8.0,
            ),
            (
                "2026-09-29T15:01:00+00:00".to_owned(),
                "get".to_owned(),
                1,
                4.0,
                4.0,
            ),
            (
                "2026-09-29T15:00:00+00:00".to_owned(),
                "put".to_owned(),
                1,
                3.0,
                3.0,
            ),
        ];

        let series = group_rows(rows);
        assert_eq!(series.len(), 2);
        assert_eq!(series[0].label, "get");
        assert_eq!(series[0].points.len(), 2);
        assert_eq!(series[0].points[0].avg, 5.0);
        assert_eq!(series[1].label, "put");
    }

    #[test]
    fn no_rows_is_no_series() {
        assert!(group_rows(vec![]).is_empty());
    }
}
