/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Native, zero-nix `flake.lock` model and updater.
//!
//! Parses a `flake.lock`, bumps tracked inputs to their newest revisions with a
//! natively recomputed `narHash`, and emits a [`Patch`] behind the
//! [`PatchGenerator`] trait so a future `updateScript` generator drops in
//! without reworking consumers.

pub mod generator;
pub mod lock;
pub mod narhash;
pub mod resolver;

pub use generator::{BumpedInput, FileEdit, FlakeLockGenerator, InputName, Patch, PatchGenerator};
pub use lock::{FlakeLock, InputRef, LockedRef, Node};
pub use resolver::{HttpRevisionResolver, ResolvedRev, RevisionResolver};
