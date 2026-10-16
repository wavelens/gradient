/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Scores are additive, largest first. QOS_PRIORITIZED is outranking any wait, and WAIT_TIME_CAP is
//! out-budgeting the estimated time against starvation. Resource penalties are keeping doomed
//! placements out, and the estimated time is ranking everything else.

pub const ASSIGN_FLOOR: f64 = 0.0;

pub const WAIT_TIME_GAIN: f64 = 60.0;
pub const WAIT_TIME_FALLBACK_AVG_SECS: f64 = 60.0;
pub const WAIT_TIME_CAP: f64 = 4000.0;

pub const TRANSFER_LIMIT_HOLD: f64 = WAIT_TIME_CAP;

pub const RESERVE_FETCH_PENALTY: f64 = 300.0;

pub const RESCORE_MAX_ROUNDS: u32 = 4;

pub const ESTIMATED_TIME_POINTS_PER_SEC: f64 = 1.0;
pub const ESTIMATED_TIME_CAP_SECS: f64 = 3600.0;
pub const BUILD_CONTENTION_PER_BUILD: f64 = 0.04;
pub const CORE_SCORE_RATIO_MIN: f64 = 0.5;
pub const CORE_SCORE_RATIO_MAX: f64 = 2.0;
pub const STORAGE_READ_FALLBACK_MBPS: f64 = 1200.0;
pub const STORAGE_WRITE_FALLBACK_MBPS: f64 = 1120.0;
pub const STORED_PER_NAR_BYTE_FALLBACK: f64 = 1.0;
pub const PER_PATH_FALLBACK_SECS: f64 = 0.18;

pub const RESOURCE_SATURATION_PENALTY: f64 =
    5000.0 + ESTIMATED_TIME_POINTS_PER_SEC * ESTIMATED_TIME_CAP_SECS;
pub const CPU_SATURATED_PCT: f32 = 80.0;
pub const CPU_SATURATED_PCT_BUILTIN: f32 = 90.0;
pub const RAM_SATURATED_FREE_FRAC: f64 = 0.10;
pub const HELD_BUILD_RESERVES_RAM_AFTER_SECS: i64 = 60;

pub const FAIR_SHARE_WEIGHT: f64 = 500.0;

pub const QOS_PRIORITIZED: f64 = 5000.0;
pub const QOS_BUILD_REQUEST: f64 = 1000.0;
pub const QOS_IFD: f64 = 1000.0;

pub const REFERENCE_CORES: u32 = 16;
pub const REFERENCE_CORE_SCORE: f64 = 4000.0;
pub const RAM_UPPER_Z: f64 = 1.28;
pub const OWN_SAMPLE_PRIOR_WEIGHT: f64 = 2.0;
pub const MIN_SPREAD: f64 = 0.05;
pub const RAM_CORE_SLOPE_PRIOR: f64 = 0.5;
pub const BUILD_TIME_CORE_SLOPE_PRIOR: f64 = -0.5;
pub const FEATURE_PENALTY: f64 = 1.0;
pub const CORE_FEATURE_PENALTY: f64 = 20.0;
pub const CORE_SLOPE_PENALTY: f64 = 50.0;
pub const FIT_EPOCHS: usize = 25;
pub const RAM_FLOOR_MARGIN: f64 = 1.1;
pub const TOP_FEATURES: usize = 5;
