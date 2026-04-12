/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod handler;
pub mod messages;
pub mod scheduler;
pub mod traits;

#[cfg(test)]
mod tests;

pub use handler::proto_router;
pub use messages::{ClientMessage, PROTO_VERSION, ServerMessage};
pub use scheduler::{Scheduler, WorkerInfo};
