/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod discovery;
mod dto;
mod error;
mod filter;
pub mod groups;
pub mod users;

pub use error::{ScimError, ScimResult};
