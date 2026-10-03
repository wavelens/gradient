/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

fn nar_str(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(&(s.len() as u64).to_le_bytes());
    out.extend_from_slice(s);
    out.extend(std::iter::repeat_n(0u8, (8 - s.len() % 8) % 8));
}

pub fn single_file_nar(contents: &[u8], executable: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(contents.len() + 128);
    for token in [b"nix-archive-1".as_slice(), b"(", b"type", b"regular"] {
        nar_str(&mut out, token);
    }
    if executable {
        nar_str(&mut out, b"executable");
        nar_str(&mut out, b"");
    }
    nar_str(&mut out, b"contents");
    nar_str(&mut out, contents);
    nar_str(&mut out, b")");
    out
}
