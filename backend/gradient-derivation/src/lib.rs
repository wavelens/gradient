/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Parsing of Nix `.drv` files into the fields Gradient schedules on.

mod derivation;
mod drv_output_spec;

pub use self::derivation::*;
pub use self::drv_output_spec::DrvOutputSpec;
