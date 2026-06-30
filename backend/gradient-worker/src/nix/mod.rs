/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Nix-specific functionality: store interaction, GC roots, logging. The flake
//! evaluator lives in the shared `gradient-eval` crate (so the CLI can reuse it)
//! and is re-exported here to keep existing `crate::nix::…` paths resolving.

pub use gradient_eval::{eval_worker, wildcard_walk};

pub mod gcroots;
pub mod log;
pub mod store;
