/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod fixtures;

use super::pktline::read_ref_from_pktlines;
use super::url::{parse_git_protocol_url, parse_nix_git_url};
use crate::sources::SourceError;
use fixtures::{FAKE_SHA, FLUSH, ref_line, ref_line_with_caps};

// ── parse_nix_git_url ────────────────────────────────────────────────────

#[test]
fn parse_nix_git_url_strips_git_plus_prefix() {
    let (url, rev) = parse_nix_git_url("git+https://example.com/repo.git?rev=deadbeef").unwrap();
    assert_eq!(url, "https://example.com/repo.git");
    assert_eq!(rev, "deadbeef");
}

#[test]
fn parse_nix_git_url_without_git_plus_prefix() {
    let (url, rev) = parse_nix_git_url("https://example.com/repo.git?rev=abc123").unwrap();
    assert_eq!(url, "https://example.com/repo.git");
    assert_eq!(rev, "abc123");
}

#[test]
fn parse_nix_git_url_rev_among_multiple_query_params() {
    let (url, rev) =
        parse_nix_git_url("git+ssh://git@host/repo.git?ref=main&rev=cafef00d&shallow=1").unwrap();
    assert_eq!(url, "ssh://git@host/repo.git");
    assert_eq!(rev, "cafef00d");
}

#[test]
fn parse_nix_git_url_missing_query_rejected() {
    assert!(matches!(
        parse_nix_git_url("git+https://example.com/repo.git"),
        Err(SourceError::UrlParsing)
    ));
}

#[test]
fn parse_nix_git_url_missing_rev_rejected() {
    assert!(matches!(
        parse_nix_git_url("git+https://example.com/repo.git?ref=main"),
        Err(SourceError::MissingHash)
    ));
}

#[test]
fn parse_nix_git_url_empty_query_missing_rev() {
    assert!(matches!(
        parse_nix_git_url("git+https://example.com/repo.git?"),
        Err(SourceError::MissingHash)
    ));
}

// ── parse_git_protocol_url ───────────────────────────────────────────────

#[test]
fn parse_git_protocol_url_default_port() {
    let (host, port, path) = parse_git_protocol_url("git://server.example.com/repo.git").unwrap();
    assert_eq!(host, "server.example.com");
    assert_eq!(port, 9418);
    assert_eq!(path, "repo.git");
}

#[test]
fn parse_git_protocol_url_explicit_port() {
    let (host, port, path) =
        parse_git_protocol_url("git://server.example.com:9419/foo/bar.git").unwrap();
    assert_eq!(host, "server.example.com");
    assert_eq!(port, 9419);
    assert_eq!(path, "foo/bar.git");
}

#[test]
fn parse_git_protocol_url_unparseable_port_falls_back_to_default() {
    let (host, port, path) =
        parse_git_protocol_url("git://server.example.com:not-a-port/repo").unwrap();
    assert_eq!(host, "server.example.com");
    assert_eq!(port, 9418);
    assert_eq!(path, "repo");
}

#[test]
fn parse_git_protocol_url_wrong_scheme_rejected() {
    assert!(matches!(
        parse_git_protocol_url("https://server/repo"),
        Err(SourceError::InvalidUrl)
    ));
}

#[test]
fn parse_git_protocol_url_missing_path_rejected() {
    assert!(matches!(
        parse_git_protocol_url("git://server.example.com"),
        Err(SourceError::InvalidUrl)
    ));
}

// ── read_ref_from_pktlines (HEAD) ─────────────────────────────────────────

#[test]
fn read_head_from_pktlines_basic() {
    let mut buf = Vec::new();
    buf.extend_from_slice(&ref_line_with_caps(FAKE_SHA, "HEAD", "multi_ack"));
    buf.extend_from_slice(&ref_line(FAKE_SHA, "refs/heads/main"));
    buf.extend_from_slice(FLUSH);

    let result = read_ref_from_pktlines(&mut buf.as_slice(), None).unwrap();
    assert_eq!(hex::encode(&result), FAKE_SHA);
}

#[test]
fn read_head_from_pktlines_head_not_first() {
    let other_sha = "1111111111111111111111111111111111111111";
    let mut buf = Vec::new();
    buf.extend_from_slice(&ref_line_with_caps(other_sha, "refs/heads/main", "caps"));
    buf.extend_from_slice(&ref_line(FAKE_SHA, "HEAD"));
    buf.extend_from_slice(FLUSH);

    let result = read_ref_from_pktlines(&mut buf.as_slice(), None).unwrap();
    assert_eq!(hex::encode(&result), FAKE_SHA);
}

#[test]
fn read_head_from_pktlines_no_head_falls_back_to_first_ref() {
    let mut buf = Vec::new();
    buf.extend_from_slice(&ref_line_with_caps(FAKE_SHA, "refs/heads/main", "caps"));
    buf.extend_from_slice(FLUSH);

    let result = read_ref_from_pktlines(&mut buf.as_slice(), None).unwrap();
    assert_eq!(hex::encode(&result), FAKE_SHA);
}

#[test]
fn read_head_from_pktlines_empty_repo_returns_error() {
    let zero_id = "0000000000000000000000000000000000000000";
    let mut buf = Vec::new();
    buf.extend_from_slice(&ref_line_with_caps(zero_id, "capabilities^{}", "multi_ack"));
    buf.extend_from_slice(FLUSH);

    let err = read_ref_from_pktlines(&mut buf.as_slice(), None).unwrap_err();
    assert!(matches!(err, SourceError::GitHashExtraction));
}

/// Reproduces the original bug: git-daemon keeps the connection open after
/// the ref advertisement. With `read_to_end` this would block until timeout
/// and then fail with EAGAIN. With incremental pkt-line reading it should
/// return HEAD immediately after the flush packet, without reading further.
#[test]
fn read_head_from_pktlines_server_keeps_connection_open() {
    use std::io::Write;
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        // Send ref advertisement then flush - but do NOT close the connection.
        let mut payload = Vec::new();
        payload.extend_from_slice(&ref_line_with_caps(FAKE_SHA, "HEAD", "multi_ack"));
        payload.extend_from_slice(FLUSH);
        conn.write_all(&payload).unwrap();
        conn.flush().unwrap();
        // Keep connection open - sleep long enough that read_to_end would block.
        std::thread::sleep(std::time::Duration::from_secs(5));
        drop(conn);
    });

    let mut stream = std::net::TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .unwrap();

    // This must return quickly (not block for 2+ seconds waiting for EOF/timeout).
    let start = std::time::Instant::now();
    let result = read_ref_from_pktlines(&mut stream, None).unwrap();
    let elapsed = start.elapsed();

    assert_eq!(hex::encode(&result), FAKE_SHA);
    assert!(
        elapsed.as_millis() < 1000,
        "read_ref_from_pktlines blocked for {}ms - likely still using read_to_end",
        elapsed.as_millis()
    );

    drop(stream);
    server.join().unwrap();
}
