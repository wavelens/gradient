/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod handler;
pub mod messages;
pub mod outbound;
pub mod session;
pub mod traits;

#[cfg(test)]
mod tests;

pub use handler::{ProtoLimiter, proto_router};
pub use messages::{ClientMessage, PROTO_VERSION, ServerMessage};

// Re-export from the scheduler crate for backward compatibility.
pub use scheduler::Scheduler;
pub use scheduler::WorkerInfo;
