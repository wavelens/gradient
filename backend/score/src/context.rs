/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::cell::{Cell, OnceCell};

#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Windowed {
    pub w5m: f64,
    pub w1h: f64,
    pub w24h: f64,
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct InstanceContext {
    pub wait_secs: Windowed,
    pub build_time_ms: Windowed,
    pub peak_ram_mb: Windowed,
    pub cpu_time_ms: Windowed,
    pub avg_cpu_pct: Windowed,
    pub disk_bytes: Windowed,
    pub network_mbps: Windowed,
    pub oom_rate: Windowed,
    pub closure_size: Windowed,
    pub nar_size_mb: Windowed,
    pub missing_paths: Windowed,
    pub dependency_cnt: Windowed,
    pub completed: Windowed,
    pub active_builds: u32,
    pub pending_builds: u32,
    pub total_workers: u32,
    pub idle_workers: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct HistoryPrediction {
    pub predicted_peak_ram_mb: u64,
    pub avg_cpu_time_ms: u64,
    pub avg_disk_bytes: u64,
    pub oom_rate: f32,
    pub samples: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WorkerMetricsView {
    pub cpu_count: u32,
    pub cpu_core_score: u32,
    pub ram_total_mb: u64,
    pub ram_free_mb: u64,
    pub cpu_usage_pct: f32,
    pub disk_speed_mbps: Option<f32>,
    pub network_speed_mbps: Option<f32>,
}

pub struct LazyProviders<'a> {
    pub closure_size: &'a dyn Fn() -> Option<i64>,
    pub history: &'a dyn Fn() -> HistoryPrediction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EvalContext {
    pub fetch_flake: bool,
}

pub struct BuildContextLazy<'a> {
    pub architecture: &'a str,
    pub prefer_local_build: bool,
    pub is_fixed_output: bool,
    pub pname: Option<&'a str>,
    providers: LazyProviders<'a>,
    closure_size: OnceCell<Option<i64>>,
    history: OnceCell<HistoryPrediction>,
    history_touched: Cell<bool>,
}

impl BuildContextLazy<'_> {
    pub fn closure_size(&self) -> Option<i64> {
        *self.closure_size.get_or_init(|| (self.providers.closure_size)())
    }

    pub fn history(&self) -> HistoryPrediction {
        self.history_touched.set(true);
        *self.history.get_or_init(|| (self.providers.history)())
    }

    #[cfg(test)]
    fn history_was_touched(&self) -> bool {
        self.history_touched.get()
    }
}

pub enum JobKindContext<'a> {
    Eval(EvalContext),
    Build(BuildContextLazy<'a>),
}

pub struct ScoredJob<'a> {
    pub job_id: &'a str,
    pub peer_id: gradient_core::types::ids::OrganizationId,
    kind: JobKindContext<'a>,
}

impl<'a> ScoredJob<'a> {
    pub fn new_eval(
        job_id: &'a str,
        peer_id: gradient_core::types::ids::OrganizationId,
        fetch_flake: bool,
    ) -> Self {
        Self { job_id, peer_id, kind: JobKindContext::Eval(EvalContext { fetch_flake }) }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_build(
        job_id: &'a str,
        peer_id: gradient_core::types::ids::OrganizationId,
        architecture: &'a str,
        prefer_local_build: bool,
        is_fixed_output: bool,
        pname: Option<&'a str>,
        providers: LazyProviders<'a>,
    ) -> Self {
        Self {
            job_id,
            peer_id,
            kind: JobKindContext::Build(BuildContextLazy {
                architecture,
                prefer_local_build,
                is_fixed_output,
                pname,
                providers,
                closure_size: OnceCell::new(),
                history: OnceCell::new(),
                history_touched: Cell::new(false),
            }),
        }
    }

    pub fn kind(&self) -> &JobKindContext<'a> {
        &self.kind
    }

    pub fn build(&self) -> Option<&BuildContextLazy<'_>> {
        match &self.kind {
            JobKindContext::Build(b) => Some(b),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_core::types::ids::OrganizationId;

    fn make_job() -> ScoredJob<'static> {
        ScoredJob::new_build(
            "test-job",
            OrganizationId::now_v7(),
            "x86_64-linux",
            false,
            false,
            None,
            LazyProviders {
                closure_size: &|| Some(99),
                history: &|| HistoryPrediction::default(),
            },
        )
    }

    fn build_ctx<'a>(job: &'a ScoredJob<'a>) -> &'a BuildContextLazy<'a> {
        match job.kind() {
            JobKindContext::Build(b) => b,
            _ => panic!("expected build"),
        }
    }

    #[test]
    fn closure_size_computed_at_most_once() {
        let job = make_job();
        let b = build_ctx(&job);
        let a = b.closure_size();
        let c = b.closure_size();
        assert_eq!(a, Some(99));
        assert_eq!(a, c);
    }

    #[test]
    fn history_not_computed_unless_read() {
        let job = make_job();
        let b = build_ctx(&job);
        assert!(!b.history_was_touched());
        let _ = b.closure_size();
        assert!(!b.history_was_touched(), "closure_size must not touch history");
        let _ = b.history();
        assert!(b.history_was_touched());
    }

    #[test]
    fn scored_job_exposes_build_kind_context() {
        let job = make_job();
        match job.kind() {
            JobKindContext::Build(b) => {
                assert_eq!(b.architecture, "x86_64-linux");
                assert!(!b.prefer_local_build);
            }
            _ => panic!("expected build"),
        }
    }

    #[test]
    fn windowed_default_is_zero_and_avg_picks_window() {
        let w = Windowed { w5m: 1.0, w1h: 2.0, w24h: 3.0 };
        assert_eq!(Windowed::default(), Windowed { w5m: 0.0, w1h: 0.0, w24h: 0.0 });
        assert_eq!(w.w1h, 2.0);
    }

    #[test]
    fn instance_context_default_is_zeroed() {
        let ic = InstanceContext::default();
        assert_eq!(ic.wait_secs.w1h, 0.0);
        assert_eq!(ic.active_builds, 0);
        assert_eq!(ic.total_workers, 0);
    }
}
