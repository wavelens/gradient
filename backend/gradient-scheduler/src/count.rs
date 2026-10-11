/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub(crate) fn clamp_to_u32(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}
