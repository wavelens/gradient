/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use base64::Engine as _;
use rand::RngExt as _;

pub fn issue() -> String {
    let mut raw = [0u8; 48];
    rand::rng().fill(&mut raw);
    base64::engine::general_purpose::STANDARD.encode(raw)
}
