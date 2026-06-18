/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Full structured views of the worker and job scoring context, serialized
//! onto the dispatched-job record so the frontend can show every collected value.

use gradient_types::proto::{FlakeTask, GradientCapabilities};
use gradient_score::{DerivationRef, HistoryPrediction, JobContext, WorkerContext};
use serde::Serialize;

use crate::jobs::PendingJob;

#[derive(Serialize)]
pub struct WorkerContextView {
    pub architectures: Vec<String>,
    pub system_features: Vec<String>,
    pub capabilities: GradientCapabilities,
    pub cpu_count: u32,
    pub cpu_core_score: u32,
    pub ram_total_mb: u64,
    pub ram_free_mb: u64,
    pub cpu_usage_pct: f32,
    pub disk_speed_mbps: Option<f32>,
    pub network_speed_mbps: Option<f32>,
}

impl WorkerContextView {
    pub fn new(w: &WorkerContext<'_>, capabilities: GradientCapabilities) -> Self {
        let m = w.metrics.unwrap_or_default();
        Self {
            architectures: w.architectures.to_vec(),
            system_features: w.system_features.to_vec(),
            capabilities,
            cpu_count: m.cpu_count,
            cpu_core_score: m.cpu_core_score,
            ram_total_mb: m.ram_total_mb,
            ram_free_mb: m.ram_free_mb,
            cpu_usage_pct: m.cpu_usage_pct,
            disk_speed_mbps: m.disk_speed_mbps,
            network_speed_mbps: m.network_speed_mbps,
        }
    }
}

#[derive(Serialize)]
pub struct HistoryView {
    pub peak_ram_mb: u64,
    pub avg_cpu_time_ms: u64,
    pub build_time_ms: u64,
    pub avg_disk_bytes: u64,
    pub oom_rate: f32,
    pub samples: u32,
}

impl From<&HistoryPrediction> for HistoryView {
    fn from(h: &HistoryPrediction) -> Self {
        Self {
            peak_ram_mb: h.predicted_peak_ram_mb,
            avg_cpu_time_ms: h.avg_cpu_time_ms,
            build_time_ms: h.build_time_ms,
            avg_disk_bytes: h.avg_disk_bytes,
            oom_rate: h.oom_rate,
            samples: h.samples,
        }
    }
}

#[derive(Serialize)]
pub struct JobContextView {
    pub kind: &'static str,
    pub architecture: String,
    pub missing_count: Option<u32>,
    pub missing_nar_size: Option<u64>,
    pub org_work_share: Option<f32>,
    pub rescore_count: u32,
    pub queued_at: chrono::NaiveDateTime,
    pub ready_at: chrono::NaiveDateTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependency_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closure_size: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefer_local_build: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_fixed_output: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<HistoryView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub derivations: Option<Vec<DerivationRef>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetch_flake: Option<bool>,
}

impl JobContextView {
    pub fn new(ctx: &JobContext<'_>, job: &PendingJob) -> Self {
        let common = |kind, architecture| Self {
            kind,
            architecture,
            missing_count: ctx.missing_count,
            missing_nar_size: ctx.missing_nar_size,
            org_work_share: ctx.org_work_share,
            rescore_count: ctx.rescore_count,
            queued_at: ctx.queued_at,
            ready_at: ctx.ready_at,
            dependency_count: None,
            pname: None,
            closure_size: None,
            prefer_local_build: None,
            is_fixed_output: None,
            history: None,
            derivations: None,
            fetch_flake: None,
        };

        match job {
            PendingJob::Build(b) => Self {
                dependency_count: Some(ctx.dependency_count),
                pname: b.pname.clone(),
                closure_size: b.closure_size,
                prefer_local_build: Some(b.prefer_local_build),
                is_fixed_output: Some(b.is_fixed_output),
                history: Some((&b.history).into()),
                derivations: Some(
                    b.job
                        .builds
                        .iter()
                        .map(|t| DerivationRef {
                            build_id: t.build_id.clone(),
                            drv_path: gradient_types::StorePath::parse(&t.drv_path)
                                .map(|sp| sp.base())
                                .unwrap_or_else(|_| t.drv_path.clone()),
                            pname: b.pname.clone(),
                        })
                        .collect(),
                ),
                ..common("Build", b.architecture.clone())
            },
            PendingJob::Eval(e) => Self {
                fetch_flake: Some(e.job.tasks.contains(&FlakeTask::FetchFlake)),
                ..common("Eval", String::new())
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_types::ids::{BuildId, EvaluationId, OrganizationId};
    use gradient_types::proto::{BuildJob, BuildTask};
    use gradient_score::ScoredJob;

    fn build_pending() -> PendingJob {
        let now = gradient_types::now();
        PendingJob::Build(crate::jobs::PendingBuildJob {
            build_id: BuildId::now_v7(),
            evaluation_id: EvaluationId::now_v7(),
            peer_id: OrganizationId::now_v7(),
            job: BuildJob {
                builds: vec![BuildTask {
                    build_id: "b1".into(),
                    drv_path: "/nix/store/aaa.drv".into(),
                    external_cached: false,
                    timeout_secs: None,
                    max_silent_secs: None,
                }],
            },
            required_paths: vec![],
            architecture: "x86_64-linux".into(),
            required_features: vec![],
            dependency_count: 3,
            closure_size: Some(42),
            prefer_local_build: true,
            is_fixed_output: false,
            history: HistoryPrediction { samples: 7, ..Default::default() },
            queued_at: now,
            ready_at: now,
            rescore_count: 0,
            pname: Some("curl".into()),
            substitute: false,
        })
    }

    #[test]
    fn build_job_context_view_carries_derivations_and_history() {
        let job = build_pending();
        let scored = ScoredJob::new_eval("build:x", job.peer_id(), false, Default::default());
        let now = gradient_types::now();
        let ctx = JobContext {
            job: &scored,
            missing_count: Some(2),
            missing_nar_size: Some(100),
            dependency_count: 3,
            queued_at: now,
            ready_at: now,
            org_work_share: None,
            rescore_count: 0,
        };
        let view = JobContextView::new(&ctx, &job);
        assert_eq!(view.kind, "Build");
        assert_eq!(view.pname.as_deref(), Some("curl"));
        let derivations = view.derivations.expect("build derivations");
        assert_eq!(derivations.len(), 1);
        assert_eq!(derivations[0].build_id, "b1");
        assert_eq!(derivations[0].drv_path, "aaa.drv");
        let history = view.history.expect("build history");
        assert_eq!(history.samples, 7);
    }
}
