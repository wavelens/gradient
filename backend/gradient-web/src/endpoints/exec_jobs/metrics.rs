/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{Caller, ProjectItemAccessRequirement};
use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::{WebError, WebResult};
use crate::extractors::{Path, Query};
use crate::helpers::ok_json;
use crate::resource::Resource;
use axum::extract::State;
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::exec_jobs::exec_job_metrics::{
    ExecAttemptResourceUsageResult, get_downsampled_usage_points_and_peaks_for_exec_attempt,
};
use gradient_types::*;
use serde::Deserialize;
use std::sync::Arc;

use super::ExecJobAccessContext;
use super::query::spec_of;
pub use gradient_api_types::{ExecJobMetricsResponse, ExecUsageChartPoint};

const BYTES_PER_MEBIBYTE: u64 = 1024 * 1024;

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ExecJobMetricsQuery {
    pub attempt: Option<u32>,
}

#[utoipa::path(
    get,
    path = "/exec/{job}/metrics",
    operation_id = "getExecJobMetrics",
    tag = "exec-jobs",
    summary = "Get metrics of exec job",
    description = "Return CPU, RAM and disk usage of one attempt. The target is the latest attempt. ``attempt`` can select a different attempt by number. Multi-node jobs respond on node 0. ``attempt_open`` is true until the attempt is complete. The requested size is the guest size of the job. The available size is the memory and disk that the guest last reported. The available size is empty before the first report. ``cpu_avg``, ``cpu_max``, ``ram_max_bytes`` and ``disk_max_bytes`` cover the whole attempt. CPU values give the count of CPUs in use, not a percentage. ``points`` can hold at most 300 points. Each point can include ``elapsed_ms`` since program start and the average and maximum of reports in that time range. Empty before program start. Return 404 for an attempt number not in the job.\n\n**Access:** Public projects, or members of the project.",
    params(("job" = ExecJobId, Path), ExecJobMetricsQuery),
    responses((status = OK, description = "Exec job metrics", body = ExecJobMetricsResponse), WebError)
)]
pub async fn get_exec_job_metrics(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(job): Path<ExecJobId>,
    Query(query): Query<ExecJobMetricsQuery>,
) -> WebResult<Json<BaseResponse<ExecJobMetricsResponse>>> {
    let ctx = ExecJobAccessContext::load(
        &state.web_db,
        job,
        Caller::from(&maybe_user),
        api_key.as_ref(),
        ProjectItemAccessRequirement::PublicProjectOrCanViewProject,
    )
    .await?;
    let attempts = &ctx.detail.attempts;
    let attempt = match query.attempt {
        Some(number) => attempts
            .iter()
            .find(|a| u32::try_from(a.number).ok() == Some(number)),
        None => attempts.iter().max_by_key(|a| a.number),
    }
    .ok_or_else(|| WebError::not_found(Resource::ExecJobAttempt))?;
    let usage = match attempt.program_started_at {
        Some(program_started_at) => {
            get_downsampled_usage_points_and_peaks_for_exec_attempt(
                &state.web_db,
                attempt.id,
                program_started_at,
                attempt.ended_at.unwrap_or_else(gradient_types::now),
            )
            .await?
        }
        None => ExecAttemptResourceUsageResult::default(),
    };
    let guest = spec_of(&ctx.detail.job).guest;
    Ok(ok_json(ExecJobMetricsResponse {
        attempt: u32::try_from(attempt.number).unwrap_or_default(),
        attempt_open: attempt.ended_at.is_none(),
        requested_cpus: guest.cpus,
        requested_memory_bytes: guest.memory_mb.saturating_mul(BYTES_PER_MEBIBYTE),
        requested_disk_bytes: guest.disk_mb.saturating_mul(BYTES_PER_MEBIBYTE),
        usable_memory_bytes: usage.ram_total_bytes,
        usable_disk_bytes: usage.disk_total_bytes,
        cpu_avg: usage.peaks.cpu_avg,
        cpu_max: usage.peaks.cpu_max,
        ram_max_bytes: usage.peaks.ram_max_bytes,
        disk_max_bytes: usage.peaks.disk_max_bytes,
        points: usage.points.into_iter().map(Into::into).collect(),
    }))
}
