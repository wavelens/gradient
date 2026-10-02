/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod driver;
pub(crate) mod eval_stats;
mod memory;
mod pool;
mod resolver;
mod transport;

pub use self::memory::{budgeted_pool_size, memory_guard_bytes};
pub use self::resolver::WorkerPoolResolver;
