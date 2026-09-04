/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! What the scheduler still owns of the build lifecycle: the orphaned-job
//! requeue and its eval-dispatch budget. Every anchor state change itself is a
//! `Transition` message to the graph actor.

mod lifecycle;

pub use crate::waiting_state::reconcile_waiting_state;
pub use lifecycle::requeue_orphaned_jobs;
