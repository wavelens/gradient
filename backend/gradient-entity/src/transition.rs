/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition<S: 'static> {
    pub from: &'static [S],
    pub to: S,
}
