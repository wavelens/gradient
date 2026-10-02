/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod handler;
pub mod import;
pub mod outbound;

/// A linker is dropping an unreferenced rlib, registry entries included. This anchor is keeping
/// the `gradient_db::sql!` statements in the plan gate's registry.
pub const fn link() {}

pub use handler::{SessionsHandle, proto_router};

pub use gradient_scheduler::Scheduler;
