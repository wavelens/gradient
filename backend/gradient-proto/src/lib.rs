/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! gradient-server's side of the Gradient protocol: the session loop,
//! dispatch table, the state-coupled NAR/cache/eval-cache handlers and the
//! outbound dial loop to discoverable workers. The wire itself lives in
//! `gradient-wire`.

pub mod handler;
pub mod ingest;
pub mod outbound;
pub mod signing;

/// Pulls this crate into a binary that otherwise references nothing from it, so
/// the statements it declares with `gradient_db::sql!` reach the plan gate's
/// registry. A linker drops an rlib nothing mentions, registry entries included.
pub const fn link() {}

pub use handler::{SessionsHandle, proto_router};

pub use gradient_scheduler::Scheduler;
