/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `GET /api/v1/admin/workers` — re-exports the existing handler so the route
//! lives under the admin namespace.

pub use crate::endpoints::workers::get_workers;
