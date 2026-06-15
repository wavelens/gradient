/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub(crate) mod eval_stats;
mod pool;
mod resolver;

pub use self::pool::budgeted_pool_size;
pub use self::resolver::WorkerPoolResolver;
