/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use serde::{Deserialize, Serialize};

pub fn metrics_enabled() -> bool {
    std::env::var("GRADIENT_WORKER_EVAL_METRICS")
        .map(|v| v != "false" && v != "0")
        .unwrap_or(true)
}

/// Counters are diffs since the worker's prior request.
/// `gc_heap_size` is the current gauge at report time.
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
