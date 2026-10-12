/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod download;
pub mod import;
pub mod memory;

use std::sync::{Mutex, MutexGuard, PoisonError};

pub use self::download::{DownloadSink, forward_nix_log};
pub use self::import::{ImportBuilder, RefuseImports};
pub use self::memory::{MemoryGuard, memory_guard_bytes, min_free_ram_guard, total_memory_bytes};

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
