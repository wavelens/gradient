/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! What the scheduler still owns of the build lifecycle: the orphaned-job
//! requeue and its eval-dispatch budget. Every shared build state change itself is a
//! `Transition` message to the graph writer.

mod lifecycle;

pub(crate) use crate::waiting_state::refresh_waiting_state;
pub(crate) use lifecycle::requeue_cluster_members;
pub use lifecycle::requeue_orphaned_jobs;
