/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

/// One fleet metric aggregated over three trailing windows. `None` means the
/// window had no samples - distinct from a measured zero, which is honored
/// instead of silently swapping in a rule's fallback constant.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Windowed {
    pub w5m: Option<f64>,
    pub w1h: Option<f64>,
    pub w24h: Option<f64>,
}

impl Windowed {
    pub fn w1h_or(self, fallback: f64) -> f64 {
        self.w1h.unwrap_or(fallback)
    }

    pub fn w24h_or(self, fallback: f64) -> f64 {
        self.w24h.unwrap_or(fallback)
    }
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

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HistoryPrediction {
    pub predicted_peak_ram_mb: u64,
    pub avg_cpu_time_ms: u64,
    pub build_time_ms: u64,
    pub avg_disk_bytes: u64,
    pub oom_rate: f32,
    pub samples: u32,
}

/// Static caps plus the latest live heartbeat of one worker. The live fields
/// (`ram_free_mb`, `cpu_usage_pct`) are `None` until the first heartbeat -
/// rules skip their clauses rather than scoring a fake zero.
#[derive(Clone, Copy, Debug, Default)]
pub struct WorkerMetricsView {
    pub cpu_count: u32,
    pub cpu_core_score: u32,
    pub ram_total_mb: u64,
    pub ram_free_mb: Option<u64>,
    pub cpu_usage_pct: Option<f32>,
    pub disk_speed_mbps: Option<f32>,
    pub network_speed_mbps: Option<f32>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DerivationRef {
    pub build_id: String,
    pub drv_path: String,
    pub pname: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BuildContext {
    pub architecture: String,
    pub dependency_count: u32,
    pub prefer_local_build: bool,
    pub is_fixed_output: bool,
    pub pname: Option<String>,
    pub closure_size: Option<i64>,
    pub history: HistoryPrediction,
    pub derivations: Vec<DerivationRef>,
}

impl BuildContext {
    pub fn aggregate(items: &[BuildContext]) -> BuildContext {
        if items.len() == 1 {
            return items[0].clone();
        }
        let first = &items[0];
        let mut out = first.clone();
        out.dependency_count = items.iter().map(|i| i.dependency_count).sum();
        out.closure_size = Some(items.iter().filter_map(|i| i.closure_size).sum());
        out.prefer_local_build = items.iter().any(|i| i.prefer_local_build);
        out.is_fixed_output = items.iter().any(|i| i.is_fixed_output);
        out.pname = if items.iter().all(|i| i.pname == first.pname) {
            first.pname.clone()
        } else {
            None
        };
        out.history.predicted_peak_ram_mb =
            items.iter().map(|i| i.history.predicted_peak_ram_mb).max().unwrap_or(0);
        out.history.oom_rate = items.iter().map(|i| i.history.oom_rate).fold(0.0, f32::max);
        out.history.avg_cpu_time_ms = items.iter().map(|i| i.history.avg_cpu_time_ms).sum();
        out.history.build_time_ms = items.iter().map(|i| i.history.build_time_ms).max().unwrap_or(0);
        out.history.avg_disk_bytes = items.iter().map(|i| i.history.avg_disk_bytes).sum();
        out.history.samples = items.iter().map(|i| i.history.samples).min().unwrap_or(0);
        out.derivations = items.iter().flat_map(|i| i.derivations.clone()).collect();
        out
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EvalContext {
    pub fetch_flake: bool,
    pub history: HistoryPrediction,
}

/// The build-side scoring view of one candidate. Values are owned - the
/// dispatch path always has closure size and history materialized on the
/// pending job, so there is nothing to compute lazily.
pub struct ScoredBuild<'a> {
    pub architecture: &'a str,
    pub prefer_local_build: bool,
    pub is_fixed_output: bool,
    pub pname: Option<&'a str>,
    closure_size: Option<i64>,
    history: HistoryPrediction,
}

impl ScoredBuild<'_> {
    pub fn closure_size(&self) -> Option<i64> {
        self.closure_size
    }

    pub fn history(&self) -> HistoryPrediction {
        self.history
    }
}

pub enum JobKindContext<'a> {
    Eval(EvalContext),
    Build(ScoredBuild<'a>),
}

pub struct ScoredJob<'a> {
    pub job_id: &'a str,
    pub org_id: gradient_types::ids::OrganizationId,
    kind: JobKindContext<'a>,
}

impl<'a> ScoredJob<'a> {
    pub fn new_eval(
        job_id: &'a str,
        org_id: gradient_types::ids::OrganizationId,
        fetch_flake: bool,
        history: HistoryPrediction,
    ) -> Self {
        Self { job_id, org_id, kind: JobKindContext::Eval(EvalContext { fetch_flake, history }) }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_build(
        job_id: &'a str,
        org_id: gradient_types::ids::OrganizationId,
        architecture: &'a str,
        prefer_local_build: bool,
        is_fixed_output: bool,
        pname: Option<&'a str>,
        closure_size: Option<i64>,
        history: HistoryPrediction,
    ) -> Self {
        Self {
            job_id,
            org_id,
            kind: JobKindContext::Build(ScoredBuild {
                architecture,
                prefer_local_build,
                is_fixed_output,
                pname,
                closure_size,
                history,
            }),
        }
    }

    pub fn kind(&self) -> &JobKindContext<'a> {
        &self.kind
    }

    pub fn build(&self) -> Option<&ScoredBuild<'_>> {
        match &self.kind {
            JobKindContext::Build(b) => Some(b),
            _ => None,
        }
    }

    pub fn history(&self) -> HistoryPrediction {
        match &self.kind {
            JobKindContext::Eval(e) => e.history,
            JobKindContext::Build(b) => b.history(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_types::ids::OrganizationId;

    fn make_job() -> ScoredJob<'static> {
        ScoredJob::new_build(
            "test-job",
            OrganizationId::now_v7(),
            "x86_64-linux",
            false,
            false,
            None,
            Some(99),
            HistoryPrediction::default(),
        )
    }

    #[test]
    fn build_carries_owned_closure_size_and_history() {
        let job = make_job();
        let b = job.build().expect("build");
        assert_eq!(b.closure_size(), Some(99));
        assert_eq!(b.history(), HistoryPrediction::default());
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
    fn windowed_default_is_absent_and_fields_read_back() {
        let w = Windowed { w5m: Some(1.0), w1h: Some(2.0), w24h: Some(3.0) };
        assert_eq!(Windowed::default(), Windowed { w5m: None, w1h: None, w24h: None });
        assert_eq!(w.w1h, Some(2.0));
    }

    /// A window with no samples falls back; a MEASURED zero is honored - the
    /// old `0.0 == absent` heuristic silently swapped in the fallback.
    #[test]
    fn windowed_or_falls_back_only_when_absent() {
        assert_eq!(Windowed { w1h: Some(5.0), ..Default::default() }.w1h_or(9.0), 5.0);
        assert_eq!(Windowed { w1h: Some(0.0), ..Default::default() }.w1h_or(9.0), 0.0);
        assert_eq!(Windowed::default().w1h_or(9.0), 9.0);
        assert_eq!(Windowed { w24h: Some(7.0), ..Default::default() }.w24h_or(9.0), 7.0);
        assert_eq!(Windowed { w24h: Some(0.0), ..Default::default() }.w24h_or(9.0), 0.0);
        assert_eq!(Windowed::default().w24h_or(9.0), 9.0);
    }

    #[test]
    fn instance_context_default_is_absent() {
        let ic = InstanceContext::default();
        assert_eq!(ic.wait_secs.w1h, None);
        assert_eq!(ic.active_builds, 0);
        assert_eq!(ic.total_workers, 0);
    }

    #[test]
    fn aggregate_len1_is_identity_and_multi_reduces() {
        let a = BuildContext {
            architecture: "x86_64-linux".into(),
            dependency_count: 2,
            prefer_local_build: false,
            is_fixed_output: false,
            pname: Some("curl".into()),
            closure_size: Some(100),
            history: HistoryPrediction {
                predicted_peak_ram_mb: 500,
                avg_cpu_time_ms: 1000,
                build_time_ms: 0,
                avg_disk_bytes: 10,
                oom_rate: 0.1,
                samples: 5,
            },
            derivations: vec![],
        };
        assert_eq!(BuildContext::aggregate(std::slice::from_ref(&a)), a);

        let mut b = a.clone();
        b.history.predicted_peak_ram_mb = 900;
        b.history.avg_cpu_time_ms = 4000;
        b.history.samples = 2;
        b.pname = Some("git".into());
        b.prefer_local_build = true;
        let agg = BuildContext::aggregate(&[a.clone(), b]);
        assert_eq!(agg.history.predicted_peak_ram_mb, 900);
        assert_eq!(agg.history.avg_cpu_time_ms, 5000);
        assert_eq!(agg.history.samples, 2);
        assert_eq!(agg.dependency_count, 4);
        assert!(agg.prefer_local_build);
        assert_eq!(agg.pname, None);
    }
}
