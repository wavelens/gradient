/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Re-exports of `gradient_entity::ids::*` so existing `use crate::types::*`
//! call sites pick up the typed IDs without an extra import.

pub use gradient_entity::ids::*;
