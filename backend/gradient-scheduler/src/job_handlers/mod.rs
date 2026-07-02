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

mod queue;
mod assignment;
mod eval_status;
mod build_status;
mod logs;
mod abort;
