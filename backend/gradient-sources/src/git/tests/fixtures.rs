/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

fn pkt_line(data: &[u8]) -> Vec<u8> {
    let len = data.len() + 4;
    let mut pkt = format!("{:04x}", len).into_bytes();
    pkt.extend_from_slice(data);
    pkt
}

pub fn ref_line(hex_sha: &str, refname: &str) -> Vec<u8> {
    pkt_line(format!("{} {}\n", hex_sha, refname).as_bytes())
}

pub fn ref_line_with_caps(hex_sha: &str, refname: &str, caps: &str) -> Vec<u8> {
    let mut data = format!("{} {}\0{}\n", hex_sha, refname, caps).into_bytes();
    let len = data.len() + 4;
    let mut pkt = format!("{:04x}", len).into_bytes();
    pkt.append(&mut data);
    pkt
}

pub const FLUSH: &[u8] = b"0000";
pub const FAKE_SHA: &str = "aabbccddee00112233445566778899aabbccddee";
