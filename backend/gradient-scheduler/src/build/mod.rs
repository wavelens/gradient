/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Handles `BuildOutput` messages from workers and build job lifecycle.
//!
//! Split by concern:
//! - [`lifecycle`] - build output/completion/failure handling and retry policy
//! - [`self_heal`] - missing-input purge-and-requeue self-heal
//!
//! Waiting-state reconciliation ([`crate::waiting_state`]) and buildability
//! checks ([`crate::buildability`]) live in their own top-level modules but are
//! re-exported here so the public surface of `crate::build` is unchanged.

mod lifecycle;
mod self_heal;

pub use crate::waiting_state::reconcile_waiting_state;
pub(crate) use lifecycle::retry_backoff_elapsed;
pub use lifecycle::{
    handle_build_job_completed, handle_build_job_failed, handle_build_output, requeue_orphaned_jobs,
};
