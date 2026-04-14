/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Nix-specific functionality: evaluation subprocesses, flake parsing, store interaction.

pub mod eval_worker;
pub mod flake;
pub mod nix_eval;
pub mod store;
