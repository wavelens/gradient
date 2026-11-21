/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::git::git_network_limits::MAX_GIT_REF_ADVERTISEMENT_READ_BYTE_COUNT;
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

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

pub fn build_protocol_v0_ref_advertisement_pkt_lines_over_64_mib_listing_limit_with_flush_packet()
-> Vec<u8> {
    let payload = format!("{} refs/heads/{}\n", "1".repeat(40), "a".repeat(4000));
    let packet = format!("{:04x}{payload}", payload.len() + 4);
    let mut advertisement = Vec::with_capacity(MAX_GIT_REF_ADVERTISEMENT_READ_BYTE_COUNT + 8192);
    while advertisement.len() <= MAX_GIT_REF_ADVERTISEMENT_READ_BYTE_COUNT {
        advertisement.extend_from_slice(packet.as_bytes());
    }
    advertisement.extend_from_slice(b"0000");
    advertisement
}

pub fn build_v0_ref_advertisement_for_main_with_multi_ack(commit_hash: &str) -> Vec<u8> {
    let payload = format!("{commit_hash} refs/heads/main\0multi_ack\n");
    format!("{:04x}{payload}0000", payload.len() + 4).into_bytes()
}

pub fn build_git_comment_pkt_lines_with_min_byte_len(min_byte_len: usize) -> Vec<u8> {
    let payload = format!("#{}\n", "c".repeat(4000));
    let packet = format!("{:04x}{payload}", payload.len() + 4);
    let mut packets = Vec::with_capacity(min_byte_len + packet.len());
    while packets.len() < min_byte_len {
        packets.extend_from_slice(packet.as_bytes());
    }
    packets
}

pub fn spawn_loopback_tcp_listener_counting_accepted_connections() -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&accepted);
    std::thread::spawn(move || {
        loop {
            match listener.accept() {
                Ok((connection, _)) => {
                    counter.fetch_add(1, Ordering::SeqCst);
                    drop(connection);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => return,
            }
        }
    });
    (port, accepted)
}
