/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::path::Path;
use std::process::Command;

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

pub fn run_git_command_in_repository_with_fixed_author_and_return_trimmed_stdout(
    repo: &Path,
    args: &[&str],
) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

pub const FLUSH: &[u8] = b"0000";
pub const FAKE_SHA: &str = "aabbccddee00112233445566778899aabbccddee";
