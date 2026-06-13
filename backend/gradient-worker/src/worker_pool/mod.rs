/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

// Consumed by the warm-fork resolver (L2); allow dead_code until then.
#[allow(dead_code)]
pub(crate) mod fork_pool;
mod pool;
mod resolver;

pub use self::resolver::WorkerPoolResolver;
