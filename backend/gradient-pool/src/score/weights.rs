/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Scores are additive, largest first. QOS_PRIORITIZED is outranking any wait, and WAIT_TIME_CAP is
//! out-budgeting the estimated time against starvation. Resource penalties are keeping doomed
//! placements out. REALISED_OUTPUTS_BONUS and the estimated time are next, and the rest are
//! tie-breakers.

pub const ASSIGN_FLOOR: f64 = 0.0;

pub const MISSING_PATHS_CAP: f64 = 200.0;
pub const MISSING_PATHS_BASELINE_K: f64 = 2.0;
pub const MISSING_PATHS_FALLBACK_AVG: f64 = 20.0;

pub const REALISED_OUTPUTS_BONUS: f64 = 2500.0;

pub const REAL_BUILD_BONUS: f64 = 50.0;
pub const ARCHLESS_BUILTIN_BONUS: f64 = 100.0;

pub const DEPENDENCY_COUNT_CAP: f64 = 50.0;
pub const DEPENDENCY_COUNT_BASELINE_K: f64 = 2.0;
pub const DEPENDENCY_COUNT_FALLBACK_AVG: f64 = 10.0;

pub const WAIT_TIME_GAIN: f64 = 60.0;
pub const WAIT_TIME_FALLBACK_AVG_SECS: f64 = 60.0;
pub const WAIT_TIME_CAP: f64 = 4000.0;

pub const RESERVE_FETCH_PENALTY: f64 = 300.0;

pub const RESCORE_MAX_ROUNDS: u32 = 4;

pub const RESOURCE_FIT_RAM_PENALTY: f64 = 400.0;
pub const RESOURCE_FIT_MAX_OVERSHOOT: f64 = 2.0;

pub const ESTIMATED_TIME_POINTS_PER_SEC: f64 = 1.0;
pub const ESTIMATED_TIME_CAP_SECS: f64 = 3600.0;
pub const BUILD_CONTENTION_PER_BUILD: f64 = 0.04;
pub const CORE_SCORE_RATIO_MIN: f64 = 0.5;
pub const CORE_SCORE_RATIO_MAX: f64 = 2.0;
pub const STORAGE_READ_FALLBACK_MBPS: f64 = 1200.0;
pub const STORAGE_WRITE_FALLBACK_MBPS: f64 = 1120.0;
pub const STORED_PER_NAR_BYTE_FALLBACK: f64 = 1.0;

pub const RESOURCE_SATURATION_PENALTY: f64 =
    5000.0 + ESTIMATED_TIME_POINTS_PER_SEC * ESTIMATED_TIME_CAP_SECS;
pub const CPU_SATURATED_PCT: f64 = 80.0;
pub const CPU_SATURATED_PCT_BUILTIN: f64 = 90.0;
pub const RAM_SATURATED_FREE_FRAC: f64 = 0.10;
pub const RAM_FIT_HEADROOM: f64 = 1.1;

pub const PREFER_LOCAL_BONUS: f64 = 150.0;
pub const PREFER_LOCAL_MISS_PENALTY: f64 = 20.0;

pub const FAIR_SHARE_WEIGHT: f64 = 500.0;

pub const QOS_PRIORITIZED: f64 = 5000.0;
pub const QOS_BUILD_REQUEST: f64 = 1000.0;
