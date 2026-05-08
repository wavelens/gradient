/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Re-export of the org permission system. The canonical definition lives in
//! `gradient_core::permissions` so that startup seeding (in the `core` crate)
//! and request-time authorization (here in `web`) share a single source of
//! truth for capability bits.

pub use gradient_core::permissions::*;
