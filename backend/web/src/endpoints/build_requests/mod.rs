/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Build-request lifecycle endpoints (issue #234).
//!
//! Two-phase upload + dispatch: the client posts a manifest of source paths
//! to learn which BLAKE3 blobs the server is missing, streams those blobs
//! one at a time, then dispatches the request to the scheduler.

pub mod blobs;
pub mod dispatch;
pub mod manifest;
mod validation;
