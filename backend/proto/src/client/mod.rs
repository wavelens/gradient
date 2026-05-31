/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Outbound TCP+WS dial helpers. Used by gradient-worker (dialing
//! gradient-server), gradient-server (dialing discoverable workers), and
//! gradient-proxy (dialing its upstream gradient-server).
//!
//! Pairs with `proto::session::handshake::{as_peer, as_authority}` - pick
//! whichever handshake role you play after the dial.

pub mod dial;

pub use dial::{dial, dial_with_auth};
