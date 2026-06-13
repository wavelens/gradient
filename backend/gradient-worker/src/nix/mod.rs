/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Nix-specific functionality: evaluation subprocesses, flake parsing, store interaction.

pub mod eval_worker;
pub(crate) mod flake_walk;
pub mod gcroots;
pub mod log;
pub mod nix_eval;
pub mod store;

pub(crate) mod wildcard_walk;
