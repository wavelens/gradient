/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Background loops that poll the DB and enqueue jobs into the in-memory scheduler.
//!
//! Split across submodules by concern:
//! - [`background`] - consistency sweep, worker liveness, and metrics loops
//! - [`eval`] - `eval_dispatch_loop`: finds `Queued` evaluations → enqueues `FlakeJob`s
//! - [`build`] - `build_dispatch_loop`: finds ready `Queued` `derivation_build` anchors → enqueues `BuildJob`s
//!
//! `trigger_dispatch::trigger_dispatch_loop` fires polling/time triggers → creates evaluations.
//!
//! The eval/build loops are idempotent: re-enqueueing the same job_id overwrites
//! the existing entry in the `JobTracker` without harm.

use std::sync::Arc;

use super::Scheduler;

mod background;
mod build;
mod eval;

// Preserves the pub(crate) surface the flat dispatch.rs exposed; some of these
// (flake_job_for_eval_source, requeue_transient_failures) have no external caller today.
#[allow(unused_imports)]
pub(crate) use build::{dispatch_ready_builds, requeue_transient_failures};
#[allow(unused_imports)]
pub(crate) use eval::{dispatch_queued_evals, flake_job_for_eval_source, organization_id_for_eval};

/// Tick interval shared by the eval and build dispatch loops.
pub(crate) const DISPATCH_TICK_SECS: u64 = 5;

/// Spawns all dispatch loops on the shared shutdown tracker so they drain on SIGTERM.
pub fn start_dispatch_loops(scheduler: Arc<Scheduler>) {
    let shutdown = scheduler.state.shutdown.clone();
    let s1 = Arc::clone(&scheduler);
    let s2 = Arc::clone(&scheduler);
    let s3 = Arc::clone(&scheduler);
    let s4 = Arc::clone(&scheduler);
    let s5 = Arc::clone(&scheduler);
    shutdown.spawn(async move { super::trigger_dispatch::trigger_dispatch_loop(s3).await });
    shutdown.spawn(async move { eval::eval_dispatch_loop(s1).await });
    shutdown.spawn(async move { build::build_dispatch_loop(s2).await });
    shutdown.spawn(async move { background::worker_sample_loop(s4).await });
    shutdown.spawn(async move { background::instance_metrics_loop(s5).await });
    let s6 = Arc::clone(&scheduler);
    shutdown.spawn(async move { background::worker_liveness_loop(s6).await });
    let s7 = Arc::clone(&scheduler);
    shutdown.spawn(async move { background::consistency_sweep_loop(s7).await });
}
