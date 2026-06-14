/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! SCIM 2.0 (RFC 7643/7644) provisioning surface. Instance-level, bearer-token
//! authenticated, mounted at `/scim/v2` only when SCIM is configured.

mod dto;
mod error;
mod filter;
pub mod discovery;
pub mod groups;
pub mod users;

pub use error::{ScimError, ScimResult};
