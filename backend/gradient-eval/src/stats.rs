/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Eval-cache counter deltas shared by the eval-worker wire protocol and the
//! worker's per-eval stats accumulator.

use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use serde::{Deserialize, Serialize};

/// Whether per-eval metrics collection is enabled (default on). Disabling skips
/// the worker's `ev.stats()` reads and the subprocess's `NIX_SHOW_STATS`
/// counters so eval pays zero stats overhead.
pub fn metrics_enabled() -> bool {
    std::env::var("GRADIENT_WORKER_EVAL_METRICS")
        .map(|v| v != "false" && v != "0")
        .unwrap_or(true)
}

/// Per-request counter delta a worker reports after serving List/Resolve.
/// Counters are diffs since the worker's prior request; gc_heap_size is the
/// current gauge at report time. Canonical delta type, shared internally and
/// on the eval-worker wire protocol (rkyv frames; serde for the test driver).
#[derive(
    Clone, Copy, Debug, Default, Archive, RkyvSerialize, RkyvDeserialize, Serialize, Deserialize,
)]
#[rkyv(derive(Debug))]
pub struct StatsDelta {
    pub nr_thunks: u64,
    pub nr_function_calls: u64,
    pub nr_primop_calls: u64,
    pub nr_lookups: u64,
    pub alloc_bytes: u64,
    pub gc_heap_size: u64,
}
