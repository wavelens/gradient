/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::codec::Proto;
use crate::messages::{ClientMessage, ServerMessage};

pub fn shape(version: u16) -> String {
    let mut out = String::new();
    for (side, described) in [
        ("client", describe::<ClientMessage>(version)),
        ("server", describe::<ServerMessage>(version)),
    ] {
        out.push_str(side);
        out.push('\n');
        for variant in top_level(&described) {
            out.push_str(variant);
            out.push('\n');
        }
    }

    out
}

fn describe<T: Proto>(version: u16) -> String {
    let mut out = String::new();
    T::describe(version, &mut out);
    out
}

fn top_level(described: &str) -> Vec<&str> {
    let inner = &described[1..described.len() - 1];
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0usize, 0usize);
    for (i, c) in inner.char_indices() {
        match c {
            '{' | '[' | '<' | '(' => depth += 1,
            '}' | ']' | '>' | ')' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&inner[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }

    parts.push(&inner[start..]);
    parts
}
