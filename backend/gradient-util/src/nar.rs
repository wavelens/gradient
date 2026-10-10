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

pub fn single_file_nar_frame(len: u64, executable: bool) -> (Vec<u8>, Vec<u8>) {
    let mut head = Vec::with_capacity(128);
    for token in [b"nix-archive-1".as_slice(), b"(", b"type", b"regular"] {
        nar_str(&mut head, token);
    }
    if executable {
        nar_str(&mut head, b"executable");
        nar_str(&mut head, b"");
    }
    nar_str(&mut head, b"contents");
    head.extend_from_slice(&len.to_le_bytes());

    let mut tail = vec![0u8; ((8 - len % 8) % 8) as usize];
    nar_str(&mut tail, b")");
    (head, tail)
}

pub fn single_file_nar(contents: &[u8], executable: bool) -> Vec<u8> {
    let (mut out, tail) = single_file_nar_frame(contents.len() as u64, executable);
    out.extend_from_slice(contents);
    out.extend(tail);
    out
}
