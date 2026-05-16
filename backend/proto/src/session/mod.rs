/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Pure protocol primitives shared by gradient-server, gradient-worker, and
//! gradient-proxy: WebSocket frame I/O and handshake state machine.
//!
//! These modules have no dependency on axum, sea-orm, or scheduler — they
//! operate on generic readers/writers and on the wire message types from
//! `crate::messages`.

pub mod frame;
pub mod handshake;
