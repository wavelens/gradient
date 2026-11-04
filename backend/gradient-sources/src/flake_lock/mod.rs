/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod generator;
pub mod input_source;
pub mod lock;
pub mod narhash;
pub mod resolver;

pub use generator::{
    BumpedInput, FileEdit, FlakeLockGenerator, InputName, Patch, PatchGenerator, ResolveProgress,
};
pub use lock::{FlakeLock, InputRef, LockedRef, Node};
pub use resolver::{HttpRevisionResolver, ResolvedRev, RevisionResolver};
