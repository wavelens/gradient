/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Cache storage, demotion of lost outputs, upstream caches and the debug-info index.

pub mod capacity;
pub mod debug_info;
pub mod demotion;
pub mod reach;
pub mod upstream;
pub mod usage;
