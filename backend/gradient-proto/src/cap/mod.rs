/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Per-capability trait surfaces. Each capability advertised in
//! `GradientCapabilities` has a `*Client` (consumer) and a `*Server`
//! (provider) trait. Roles implement the traits matching the capabilities
//! they advertise.

pub mod build;
pub mod cache;
pub mod eval;
pub mod federate;
pub mod fetch;
