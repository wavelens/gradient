/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::estimate::BuildEstimate;

/// `None` is marking a window without samples. A measured zero is honored instead of being replaced
/// by a rule's fallback constant.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Windowed {
    pub w5m: Option<f64>,
    pub w1h: Option<f64>,
    pub w24h: Option<f64>,
}

impl InstanceContext {
    /// A build without history is expected to peak like the mean build of the instance.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "whole number clamped at zero"
    )]
    pub fn unmeasured_build_ram_mb(&self) -> u64 {
        let mean = self.peak_ram_mb.w24h.or(self.peak_ram_mb.w1h);
        mean.map_or(0, |mb| mb.max(0.0).trunc() as u64)
    }
}

impl Windowed {
    pub fn w1h_or(self, fallback: f64) -> f64 {
        self.w1h.unwrap_or(fallback)
    }

    pub fn w24h_or(self, fallback: f64) -> f64 {
        self.w24h.unwrap_or(fallback)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SubstituteCost {
    pub per_path_secs: f64,
    pub secs_per_mb: f64,
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct InstanceContext {
    pub wait_secs: Windowed,
    pub build_time_ms: Windowed,
    pub build_time_median_ms: Windowed,
    pub peak_ram_mb: Windowed,
    pub cpu_time_ms: Windowed,
    pub avg_cpu_pct: Windowed,
    pub disk_bytes: Windowed,
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
    pub cpu_core_score_mean: Option<f64>,
    pub upload_speed_mean_mbps: Option<f64>,
    pub download_speed_mean_mbps: Option<f64>,
    pub downloads_in_flight: u32,
    pub uploads_in_flight: u32,
    pub download_slots: u32,
    pub upload_slots: u32,
    pub storage_read_mbps: Option<f64>,
    pub storage_write_mbps: Option<f64>,
    pub compression_ratio: Option<f64>,
    pub per_path_secs: Option<f64>,
    pub substitute_cost: Option<SubstituteCost>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WorkerMetricsView {
    pub cpu_count: u32,
    pub build_cores: u32,
    pub cpu_core_score: u32,
    pub ram_total_mb: u64,
    pub ram_free_mb: Option<u64>,
    pub cpu_usage_pct: Option<f32>,
    pub disk_speed_mbps: Option<f32>,
    pub upload_speed_mbps: Option<f32>,
    pub download_speed_mbps: Option<f32>,
    pub running_builds: u32,
    pub ram_reserved_mb: u64,
    pub ram_reserved_unused_mb: Option<u64>,
    pub ram_reserved_for_waiting_build_mb: u64,
}

/// The RAM a job is expected to hold on its worker, reserved from the assignment to the release.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RamNeed {
    #[default]
    Negligible,
    Predicted(u64),
    Unmeasured,
}

impl RamNeed {
    pub fn of_build(predicted_peak_ram_mb: Option<u64>, substitution: bool, fetch: bool) -> Self {
        match predicted_peak_ram_mb {
            _ if substitution => Self::Negligible,
            Some(peak) => Self::Predicted(peak),
            None if fetch => Self::Negligible,
            None => Self::Unmeasured,
        }
    }

    pub fn reserved_mb(self, unmeasured_build_ram_mb: u64) -> u64 {
        match self {
            Self::Negligible => 0,
            Self::Predicted(peak) => peak,
            Self::Unmeasured => unmeasured_build_ram_mb,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RamFit {
    Fits,
    FitsOnceRamIsReleased,
    DoesNotFit,
}

impl WorkerMetricsView {
    pub fn full_cores(&self) -> u32 {
        match self.build_cores {
            0 => self.cpu_count.max(1),
            cores if self.cpu_count > 0 => cores.min(self.cpu_count),
            cores => cores,
        }
    }

    pub fn ram_fit(&self, needed_mb: u64) -> RamFit {
        if needed_mb == 0 || self.ram_available_mb().is_none_or(|free| needed_mb <= free) {
            RamFit::Fits
        } else if self.will_release_ram() && needed_mb <= self.ram_total_mb {
            RamFit::FitsOnceRamIsReleased
        } else {
            RamFit::DoesNotFit
        }
    }

    pub fn ram_unreserved_mb(&self) -> Option<u64> {
        (self.ram_total_mb > 0).then(|| self.ram_total_mb.saturating_sub(self.ram_reserved_mb))
    }

    /// A build reaches its peak minutes after its assignment. The measured free RAM alone would
    /// let a burst of assignments overcommit the worker, so the predicted peaks of the jobs
    /// already assigned are bounding it too.
    pub fn ram_available_mb(&self) -> Option<u64> {
        let free = match (self.ram_free_mb, self.ram_reserved_unused_mb) {
            (Some(free), Some(unused)) => Some(free.saturating_sub(unused)),
            (free, _) => free,
        };
        let available = match (free, self.ram_unreserved_mb()) {
            (Some(free), Some(unreserved)) => Some(free.min(unreserved)),
            (free, unreserved) => free.or(unreserved),
        };
        available.map(|mb| mb.saturating_sub(self.ram_reserved_for_waiting_build_mb))
    }

    pub fn will_release_ram(&self) -> bool {
        self.ram_reserved_mb + self.ram_reserved_for_waiting_build_mb > 0
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DerivationRef {
    pub build_id: String,
    pub drv_path: String,
    pub pname: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EvalContext<'a> {
    pub fetch_flake: bool,
    pub estimate: &'a BuildEstimate,
}

pub struct ScoredBuild<'a> {
    pub architecture: &'a str,
    pub prefer_local_build: bool,
    pub is_fixed_output: bool,
    pub pname: Option<&'a str>,
    closure_size: Option<i64>,
    estimate: &'a BuildEstimate,
}

impl<'a> ScoredBuild<'a> {
    pub fn is_fetch(&self) -> bool {
        self.is_fixed_output || self.architecture == gradient_types::BUILTIN_ARCH
    }

    pub fn closure_size(&self) -> Option<i64> {
        self.closure_size
    }

    pub fn estimate(&self) -> &'a BuildEstimate {
        self.estimate
    }
}

pub enum JobKindContext<'a> {
    Eval(EvalContext<'a>),
    Build(ScoredBuild<'a>),
}

pub struct ScoredJob<'a> {
    pub job_id: &'a str,
    pub project_id: gradient_types::ids::ProjectId,
    kind: JobKindContext<'a>,
}

impl<'a> ScoredJob<'a> {
    pub fn new_eval(
        job_id: &'a str,
        project_id: gradient_types::ids::ProjectId,
        fetch_flake: bool,
        estimate: &'a BuildEstimate,
    ) -> Self {
        Self {
            job_id,
            project_id,
            kind: JobKindContext::Eval(EvalContext {
                fetch_flake,
                estimate,
            }),
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "arg-heavy; refactor tracked in #503"
    )]
    pub fn new_build(
        job_id: &'a str,
        project_id: gradient_types::ids::ProjectId,
        architecture: &'a str,
        prefer_local_build: bool,
        is_fixed_output: bool,
        pname: Option<&'a str>,
        closure_size: Option<i64>,
        estimate: &'a BuildEstimate,
    ) -> Self {
        Self {
            job_id,
            project_id,
            kind: JobKindContext::Build(ScoredBuild {
                architecture,
                prefer_local_build,
                is_fixed_output,
                pname,
                closure_size,
                estimate,
            }),
        }
    }

    pub fn kind(&self) -> &JobKindContext<'a> {
        &self.kind
    }

    pub fn build(&self) -> Option<&ScoredBuild<'a>> {
        match &self.kind {
            JobKindContext::Build(b) => Some(b),
            _ => None,
        }
    }

    pub fn estimate(&self) -> &'a BuildEstimate {
        match &self.kind {
            JobKindContext::Eval(e) => e.estimate,
            JobKindContext::Build(b) => b.estimate(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windowed_or_falls_back_only_when_absent() {
        assert_eq!(
            Windowed {
                w1h: Some(5.0),
                ..Default::default()
            }
            .w1h_or(9.0),
            5.0
        );
        assert_eq!(
            Windowed {
                w1h: Some(0.0),
                ..Default::default()
            }
            .w1h_or(9.0),
            0.0
        );
        assert_eq!(Windowed::default().w1h_or(9.0), 9.0);
        assert_eq!(
            Windowed {
                w24h: Some(7.0),
                ..Default::default()
            }
            .w24h_or(9.0),
            7.0
        );
        assert_eq!(
            Windowed {
                w24h: Some(0.0),
                ..Default::default()
            }
            .w24h_or(9.0),
            0.0
        );
        assert_eq!(Windowed::default().w24h_or(9.0), 9.0);
    }
}
