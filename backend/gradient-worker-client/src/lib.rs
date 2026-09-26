/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The worker side of the Gradient protocol: connecting, reconnecting,
//! correlating replies and moving NARs. Shared by the worker and the proxy.

pub mod connection;
pub mod correlation;
pub mod http;
pub mod reconnect;
