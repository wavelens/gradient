/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::worker_names::worker_name_sql;
use chrono::NaiveDateTime;
use sea_orm::{ConnectionTrait, DbErr, QueryResult};
use serde::Serialize;

#[derive(Serialize, utoipa::ToSchema)]
pub struct WorkerNet {
    pub worker_id: Option<String>,
    pub worker_name: Option<String>,
    pub upload_speed_mbps: Option<f32>,
    pub download_speed_mbps: Option<f32>,
    pub disk_speed_mbps: Option<f32>,
}

impl From<QueryResult> for WorkerNet {
    fn from(r: QueryResult) -> Self {
        Self {
            worker_id: r.try_get("", "worker_id").ok(),
            worker_name: r.try_get("", "worker_name").ok(),
            upload_speed_mbps: r.try_get("", "upload_speed_mbps").ok().flatten(),
            download_speed_mbps: r.try_get("", "download_speed_mbps").ok().flatten(),
            disk_speed_mbps: r.try_get("", "disk_speed_mbps").ok().flatten(),
        }
    }
}

/// Worker telemetry is carrying no project. It is attributed to every project the worker is
/// registered in, or that grants the worker's team its workers.
fn workers_serving(project_list: &str) -> String {
    format!(
        " AND worker_id IN (\
         SELECT worker_id FROM worker_registration WHERE peer_id IN ({project_list}) \
         UNION SELECT tw.worker_id FROM team_worker tw \
         JOIN team_project tp ON tp.team = tw.team \
         WHERE tp.includes_workers AND tp.project IN ({project_list}))"
    )
}

fn board_network_sql(project_filter: Option<&str>) -> String {
    format!(
        "SELECT s.*, wn.display_name AS worker_name FROM ( \
           SELECT DISTINCT ON (worker_id) worker_id, upload_speed_mbps, download_speed_mbps, disk_speed_mbps \
           FROM worker_sample \
           WHERE at >= (now() AT TIME ZONE 'UTC') - interval '1 hour'{scope} \
           ORDER BY worker_id, at DESC \
         ) s {worker_name} \
         ORDER BY s.worker_id",
        scope = project_filter.map(workers_serving).unwrap_or_default(),
        worker_name = worker_name_sql("s.worker_id", project_filter),
    )
}

crate::sql_fn! {
    BOARD_NETWORK = || board_network_sql(Some("'11111111-1111-1111-1111-111111111111'")),
        params = [];
}

pub async fn latest_speeds<C: ConnectionTrait>(
    db: &C,
    project_filter: Option<&str>,
) -> Result<Vec<WorkerNet>, DbErr> {
    let rows = db
        .query_all_raw(BOARD_NETWORK.bind_built(board_network_sql(project_filter), []))
        .await?;

    Ok(rows.into_iter().map(WorkerNet::from).collect())
}

fn board_fleet_sql(window_hours: i64, project_filter: Option<&str>) -> String {
    let mut sql = format!(
        "SELECT date_trunc('hour', at) AS bucket, \
                count(DISTINCT worker_id) AS connected, \
                count(DISTINCT worker_id) FILTER (WHERE state = {draining}) AS draining, \
                count(DISTINCT worker_id) FILTER (WHERE (capabilities->>'eval')::boolean) AS ev, \
                count(DISTINCT worker_id) FILTER (WHERE (capabilities->>'fetch')::boolean) AS ft, \
                count(DISTINCT worker_id) FILTER (WHERE (capabilities->>'build')::boolean) AS bd \
         FROM worker_sample \
         WHERE at >= (now() AT TIME ZONE 'UTC') - interval '{window_hours} hours'",
        draining = i16::from(gradient_entity::worker_sample::WorkerSampleState::Draining),
    );

    if let Some(list) = project_filter {
        sql.push_str(&workers_serving(list));
    }

    sql.push_str(" GROUP BY bucket ORDER BY bucket");
    sql
}

crate::sql_fn! {
    BOARD_FLEET = || board_fleet_sql(24, Some("'11111111-1111-1111-1111-111111111111'")),
        params = [];
}

pub struct FleetCount {
    pub hour: NaiveDateTime,
    pub connected: i64,
    pub draining: i64,
    pub eval: i64,
    pub fetch: i64,
    pub build: i64,
}

pub async fn fleet_per_hour<C: ConnectionTrait>(
    db: &C,
    window_hours: i64,
    project_filter: Option<&str>,
) -> Result<Vec<FleetCount>, DbErr> {
    let rows = db
        .query_all_raw(BOARD_FLEET.bind_built(board_fleet_sql(window_hours, project_filter), []))
        .await?;

    Ok(rows
        .iter()
        .map(|r| FleetCount {
            hour: r.try_get("", "bucket").unwrap_or_default(),
            connected: r.try_get("", "connected").unwrap_or(0),
            draining: r.try_get("", "draining").unwrap_or(0),
            eval: r.try_get("", "ev").unwrap_or(0),
            fetch: r.try_get("", "ft").unwrap_or(0),
            build: r.try_get("", "bd").unwrap_or(0),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn worker_telemetry_is_scoped_through_the_workers_serving_the_projects() {
        for sql in [
            super::board_network_sql(Some("'p'")),
            super::board_fleet_sql(24, Some("'p'")),
        ] {
            assert!(!sql.contains(" AND project IN"), "{sql}");
            assert!(
                sql.contains("worker_registration WHERE peer_id IN ('p')"),
                "{sql}"
            );
            assert!(sql.contains("team_project tp"), "{sql}");
        }
        let network = super::board_network_sql(Some("'p'"));
        assert!(
            network.contains("wr.worker_id = s.worker_id AND wr.peer_id IN ('p')"),
            "{network}"
        );
        assert!(
            network.contains("tw.worker_id = s.worker_id AND tw.team IN"),
            "{network}"
        );
    }
}
