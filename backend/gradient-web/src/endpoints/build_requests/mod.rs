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
//!
//! [`url`] is the no-upload variant: the source is already published at a
//! remote URL, so only the URL and a revision are posted (#564).

pub mod blobs;
pub mod dispatch;
pub mod manifest;
pub mod source;
pub mod types;
pub mod url;
mod validation;
