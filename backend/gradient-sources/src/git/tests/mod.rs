/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod fixtures;

use super::checkout::checkout_commit;
use super::commit_info::{fetch_commit, head_refspec};
use super::pktline::read_ref_from_pktlines;
use super::url::{git_transport_url, parse_git_protocol_url};
use crate::SourceError;
use fixtures::{FAKE_SHA, FLUSH, ref_line, ref_line_with_caps};

#[test]
fn git_transport_url_strips_git_plus_https() {
    assert_eq!(
        git_transport_url("git+https://git.example.com/org/repo..git"),
        "https://git.example.com/org/repo..git"
    );
}

#[test]
fn git_transport_url_strips_git_plus_http_and_ssh() {
    assert_eq!(git_transport_url("git+http://h/r"), "http://h/r");
    assert_eq!(git_transport_url("git+ssh://git@h/r"), "ssh://git@h/r");
}

#[test]
fn git_transport_url_passes_through_bare_schemes_and_scp() {
    assert_eq!(git_transport_url("https://h/r"), "https://h/r");
    assert_eq!(git_transport_url("git://h/r"), "git://h/r");
    assert_eq!(
        git_transport_url("git@github.com:u/r.git"),
        "git@github.com:u/r.git"
    );
}

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

/// git-daemon is keeping the connection open after the ref advertisement. `read_to_end` was
/// blocking until timeout here.
#[test]
fn read_head_from_pktlines_server_keeps_connection_open() {
    use std::io::Write;
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let mut payload = Vec::new();
        payload.extend_from_slice(&ref_line_with_caps(FAKE_SHA, "HEAD", "multi_ack"));
        payload.extend_from_slice(FLUSH);
        conn.write_all(&payload).unwrap();
        conn.flush().unwrap();
        std::thread::sleep(std::time::Duration::from_secs(5));
        drop(conn);
    });

    let mut stream = std::net::TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .unwrap();

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

fn commit_on(repo: &git2::Repository, branch: &str, message: &str) -> git2::Oid {
    let sig = git2::Signature::now("Ada", "ada@example.com").unwrap();
    let tree = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    let parent = repo
        .find_reference(&format!("refs/heads/{branch}"))
        .ok()
        .and_then(|r| r.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    repo.commit(
        Some(&format!("refs/heads/{branch}")),
        &sig,
        &sig,
        message,
        &tree,
        &parents,
    )
    .unwrap()
}

#[test]
fn the_head_commit_is_the_tip_of_the_ref_asked_for() {
    let dir = tempfile::TempDir::new().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    repo.set_head("refs/heads/main").unwrap();
    commit_on(&repo, "main", "first");
    let main = commit_on(&repo, "main", "second\n\nbody");
    let feature = commit_on(&repo, "feature", "on feature");
    let url = format!("file://{}", dir.path().display());

    let head = fetch_commit(&url, None, &head_refspec(None)).unwrap();
    assert_eq!(head.hash, main.as_bytes());
    assert_eq!(head.message, "second");
    assert_eq!(head.author_name, "Ada");
    assert_eq!(head.author_email.as_deref(), Some("ada@example.com"));

    let branch = fetch_commit(&url, None, &head_refspec(Some("feature"))).unwrap();
    assert_eq!(branch.hash, feature.as_bytes());
}

#[test]
fn a_pinned_commit_below_the_tip_is_fetched_by_its_hash() {
    let dir = tempfile::TempDir::new().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    repo.set_head("refs/heads/main").unwrap();
    let pinned = commit_on(&repo, "main", "pinned\n\nbody");
    commit_on(&repo, "main", "tip");
    let url = format!("file://{}", dir.path().display());

    let commit = fetch_commit(&url, None, &pinned.to_string()).unwrap();
    assert_eq!(commit.hash, pinned.as_bytes());
    assert_eq!(commit.message, "pinned");
}

fn commit_file(repo: &git2::Repository, refname: &str, contents: &str) -> git2::Oid {
    let sig = git2::Signature::now("Ada", "ada@example.com").unwrap();
    let mut tree = repo.treebuilder(None).unwrap();
    tree.insert("file", repo.blob(contents.as_bytes()).unwrap(), 0o100644)
        .unwrap();
    let tree = repo.find_tree(tree.write().unwrap()).unwrap();
    let parent = repo
        .find_reference(refname)
        .ok()
        .and_then(|r| r.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    repo.commit(Some(refname), &sig, &sig, contents, &tree, &parents)
        .unwrap()
}

fn checked_out_file(url: &str, commit: git2::Oid) -> String {
    let checkout = checkout_commit(url, &commit.to_string(), None).unwrap();
    std::fs::read_to_string(checkout.path().join("file")).unwrap()
}

#[test]
fn a_commit_below_the_tip_is_checked_out_at_its_own_tree() {
    let dir = tempfile::TempDir::new().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let pinned = commit_file(&repo, "refs/heads/main", "pinned");
    commit_file(&repo, "refs/heads/main", "tip");
    let url = format!("file://{}", dir.path().display());

    assert_eq!(checked_out_file(&url, pinned), "pinned");
}

#[test]
fn a_commit_only_on_a_pull_request_ref_is_checked_out() {
    let dir = tempfile::TempDir::new().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit_file(&repo, "refs/heads/main", "main");
    let pull = commit_file(&repo, "refs/pull/1/head", "fork");
    let url = format!("file://{}", dir.path().display());

    assert_eq!(checked_out_file(&url, pull), "fork");
}

struct GitDaemon(std::process::Child);

impl Drop for GitDaemon {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

fn start_git_daemon(base: &std::path::Path) -> (GitDaemon, String) {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let exec_path = std::process::Command::new("git")
        .arg("--exec-path")
        .output()
        .unwrap()
        .stdout;
    let exec_path = std::path::PathBuf::from(String::from_utf8(exec_path).unwrap().trim());
    let daemon = std::process::Command::new(exec_path.join("git-daemon"))
        .args(["--export-all", "--reuseaddr", "--listen=127.0.0.1"])
        .arg(format!("--port={port}"))
        .arg(format!("--base-path={}", base.display()))
        .spawn()
        .unwrap();
    let daemon = GitDaemon(daemon);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            std::time::Instant::now() < deadline,
            "git daemon did not start"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    (daemon, format!("git://127.0.0.1:{port}/repo"))
}

#[test]
fn a_branch_tip_is_fetched_shallow_from_a_server_refusing_fetches_by_hash() {
    let base = tempfile::TempDir::new().unwrap();
    let repo = git2::Repository::init(base.path().join("repo")).unwrap();
    commit_file(&repo, "refs/heads/main", "first");
    let tip = commit_file(&repo, "refs/heads/main", "tip");
    let (_daemon, url) = start_git_daemon(base.path());

    let checkout = checkout_commit(&url, &tip.to_string(), None).unwrap();
    assert_eq!(
        std::fs::read_to_string(checkout.path().join("file")).unwrap(),
        "tip"
    );
    assert!(
        git2::Repository::open(checkout.path())
            .unwrap()
            .is_shallow()
    );
}

#[test]
fn a_commit_below_the_tip_is_checked_out_from_a_server_refusing_fetches_by_hash() {
    let base = tempfile::TempDir::new().unwrap();
    let repo = git2::Repository::init(base.path().join("repo")).unwrap();
    let pinned = commit_file(&repo, "refs/heads/main", "pinned");
    commit_file(&repo, "refs/heads/main", "tip");
    let (_daemon, url) = start_git_daemon(base.path());

    assert_eq!(checked_out_file(&url, pinned), "pinned");
}
