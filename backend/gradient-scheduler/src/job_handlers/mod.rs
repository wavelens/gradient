/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `Scheduler` methods for job queuing, assignment, status updates,
//! completion, log streaming, abort, and diagnostics.
//!
//! Split across submodules by concern:
//! - [`queue`] - enqueue, candidate listing, diagnostics
//! - [`assignment`] - scoring and job assignment (`RequestJob`)
//! - [`eval_status`] - eval status transitions, result handling, messages
//! - [`build_status`] - build status transitions, job completion/failure
//! - [`logs`] - log streaming
//! - [`abort`] - evaluation abort

mod abort;
mod assignment;
mod build_status;
mod eval_status;
mod logs;
mod queue;
