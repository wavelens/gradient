/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The scoring weight model, in one place. Every rule's magnitude lives here so
//! cross-rule priorities are visible and tunable side by side instead of being
//! scattered magic numbers. Scores are additive; a job dispatches to a worker
//! only when its summed total reaches [`ASSIGN_FLOOR`] and no rule vetoed.
//!
//! Relative scale (largest first): a prioritized job's QOS_PRIORITIZED (5000)
//! outranks any unprioritized wait; anti-starvation WAIT_TIME_CAP (4000)
//! out-budgets everything else so nothing waits forever; the resource penalties
//! (RESOURCE_SATURATION_PENALTY 5000, stackable to 10000; RAM overshoot up to
//! RESOURCE_FIT_RAM_PENALTY x MAX_OVERSHOOT = 800 before OOM factors) keep
//! doomed placements out; REALISED_OUTPUTS_BONUS (2500) sends a job to a worker
//! that only has to upload it; CpuAffinityRule (up to +-1200) moves long builds
//! to faster cores; cache-warmth bonuses (MISSING_NAR_SIZE_CAP 500,
//! MISSING_PATHS_CAP 200) prefer cheap transfers; the rest are tie-breakers.

/// A job dispatches only when its summed score reaches this floor. "Do not
/// dispatch yet" is expressed by a rule veto, not by hoping penalties push the
/// sum below the floor.
pub const ASSIGN_FLOOR: f64 = 0.0;

/// MissingPathsRule: bonus cap for a fully-warm worker, baseline multiplier
/// over the 1h average missing-path count, and the fallback average.
pub const MISSING_PATHS_CAP: f64 = 200.0;
pub const MISSING_PATHS_BASELINE_K: f64 = 2.0;
pub const MISSING_PATHS_FALLBACK_AVG: f64 = 20.0;

/// MissingNarSizeRule: bonus cap for zero bytes left to download and the
/// baseline multiplier over the 1h average missing-NAR megabytes.
pub const MISSING_NAR_SIZE_CAP: f64 = 500.0;
pub const MISSING_NAR_SIZE_BASELINE_K: f64 = 2.0;

/// RealisedOutputsRule: bonus for a worker already holding every output. Out-budgets
/// a rival's full cache warmth plus the largest CpuAffinityRule swing.
pub const REALISED_OUTPUTS_BONUS: f64 = 2500.0;

/// BuiltinDeprioritizeRule: bonus for real compilation jobs, and the stronger
/// lift builtins get on an architecture-less worker so it is not left idle.
pub const REAL_BUILD_BONUS: f64 = 50.0;
pub const ARCHLESS_BUILTIN_BONUS: f64 = 100.0;

/// DependencyCountRule: cap for a build with many direct inputs, baseline
/// multiplier over the 1h average input count, and the fallback average.
pub const DEPENDENCY_COUNT_CAP: f64 = 50.0;
pub const DEPENDENCY_COUNT_BASELINE_K: f64 = 2.0;
pub const DEPENDENCY_COUNT_FALLBACK_AVG: f64 = 10.0;

/// WaitTimeRule: gain per multiple of the average wait, the fallback average,
/// and the anti-starvation cap that out-budgets every other rule.
pub const WAIT_TIME_GAIN: f64 = 60.0;
pub const WAIT_TIME_FALLBACK_AVG_SECS: f64 = 60.0;
pub const WAIT_TIME_CAP: f64 = 4000.0;

/// ReserveFetchWorkersRule: penalty for spending a fetch-capable worker on a
/// cached evaluation while no workers are idle.
pub const RESERVE_FETCH_PENALTY: f64 = 300.0;

/// RescoreWaitRule: rounds a build waits for its substitution cost to be
/// measured before dispatching unmeasured (the rule vetoes until then).
pub const RESCORE_MAX_ROUNDS: u32 = 4;

/// ResourceFitRule: RAM-overshoot penalty scale and its overshoot clamp.
pub const RESOURCE_FIT_RAM_PENALTY: f64 = 400.0;
pub const RESOURCE_FIT_MAX_OVERSHOOT: f64 = 2.0;

/// CpuAffinityRule: points per heaviness step at a core twice the fleet mean,
/// the heaviness cap (1 at the heavy threshold, +1 per doubling), and the
/// heavy-work threshold when the instance has no CPU-time average. A capped
/// build on a core 50% off the mean swings more than MISSING_NAR_SIZE_CAP.
pub const CPU_AFFINITY_WEIGHT: f64 = 400.0;
pub const CPU_HEAVINESS_CAP: f64 = 3.0;
pub const CPU_HEAVY_THRESHOLD_MS: u64 = 60_000;

/// ResourceSaturationRule: flat penalty per tripped saturation signal
/// (stackable), the CPU thresholds, the free-RAM fraction floor, and the
/// headroom factor over the predicted peak.
pub const RESOURCE_SATURATION_PENALTY: f64 = 5000.0;
pub const CPU_SATURATED_PCT: f64 = 80.0;
pub const CPU_SATURATED_PCT_BUILTIN: f64 = 90.0;
pub const RAM_SATURATED_FREE_FRAC: f64 = 0.10;
pub const RAM_FIT_HEADROOM: f64 = 1.1;

/// PreferLocalBuildRule: bonus for building marked-local derivations where
/// their inputs already are, and the per-missing-path decay fallback.
pub const PREFER_LOCAL_BONUS: f64 = 150.0;
pub const PREFER_LOCAL_MISS_PENALTY: f64 = 20.0;

/// NetworkAffinityRule: bonus cap for fixed-output fetches on fast links and
/// the reference link speed when no fleet average exists.
pub const NETWORK_AFFINITY_BONUS: f64 = 80.0;
pub const NETWORK_REFERENCE_MBPS: f64 = 100.0;

/// DiskAffinityRule: bonus cap for disk-heavy builds on fast disks, the
/// heavy-build byte threshold, and the reference disk speed.
pub const DISK_AFFINITY_BONUS: f64 = 60.0;
pub const DISK_HEAVY_THRESHOLD_BYTES: u64 = 100 * 1_048_576;
pub const DISK_REFERENCE_MBPS: f64 = 500.0;

/// FairShareRule: penalty scale per unit of a project's active-build share.
pub const FAIR_SHARE_WEIGHT: f64 = 500.0;

/// QosRule: quality-of-service bonus for a prioritized job.
pub const QOS_PRIORITIZED: f64 = 5000.0;
