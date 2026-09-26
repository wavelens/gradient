/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Outbound webhooks: which events reach which webhook, how a body is signed,
//! and one delivery with its log row.

mod deliver;
mod routing;
mod sign;

pub use deliver::deliver;
pub use routing::{candidates, routes_to};
pub use sign::sign;
