/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The eval-worker subprocess pool, split along its seams: [`transport`] owns
//! one subprocess handle + the rkyv frame wire, [`pool`] the checkout/return
//! lifecycle, [`memory`] the RAM budget and reaper, [`resolver`] the pooled
//! fan-out driving it all, and [`driver`] a JSONL test harness over the lot.

pub mod driver;
pub(crate) mod eval_stats;
mod memory;
mod pool;
mod resolver;
mod transport;

pub use self::memory::{budgeted_pool_size, memory_guard_bytes};
pub use self::resolver::WorkerPoolResolver;
