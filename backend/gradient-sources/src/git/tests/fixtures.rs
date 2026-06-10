/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

/// Build a git protocol v0 pkt-line from raw bytes.
fn pkt_line(data: &[u8]) -> Vec<u8> {
    let len = data.len() + 4;
    let mut pkt = format!("{:04x}", len).into_bytes();
    pkt.extend_from_slice(data);
    pkt
}

/// Build a ref advertisement line: "<hex-sha1> <refname>\n"
pub fn ref_line(hex_sha: &str, refname: &str) -> Vec<u8> {
    pkt_line(format!("{} {}\n", hex_sha, refname).as_bytes())
}

/// The first ref line includes NUL-separated capabilities:
/// "<hex-sha1> <refname>\0<capabilities>\n"
pub fn ref_line_with_caps(hex_sha: &str, refname: &str, caps: &str) -> Vec<u8> {
    let mut data = format!("{} {}\0{}\n", hex_sha, refname, caps).into_bytes();
    // pkt_line wraps it
    let len = data.len() + 4;
    let mut pkt = format!("{:04x}", len).into_bytes();
    pkt.append(&mut data);
    pkt
}

pub const FLUSH: &[u8] = b"0000";
pub const FAKE_SHA: &str = "aabbccddee00112233445566778899aabbccddee";
