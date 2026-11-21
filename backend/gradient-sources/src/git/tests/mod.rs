/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod changed_paths;
pub(crate) mod fixtures;
mod https_credentials_for_operation_host;

use super::checkout::Origin;
use super::checkout::{
    checkout_commit, checkout_commit_into, fetch_commit_into_new_bare_temporary_repository,
};
use super::commit_info::{fetch_commit, head_refspec};
use super::git_credentials::{
    GitOperationCredentials, run_blocking_libgit2_operation_with_git_credentials,
};
use super::git_network_limits::{
    BlockingGitFetchOperationLimits, CURRENT_THREAD_BLOCKING_LIBGIT2_FETCH_LIMITS,
    current_thread_blocking_libgit2_fetch_time_and_byte_limits_or_ordinary_fetch_limits,
};
use super::pktline::{MAX_GIT_REF_ADVERTISEMENT_READ_BYTE_COUNT, read_advertised_refs};
use super::remote::{
    AdvertisedRef, BranchHead, branch_head, list_remote_refs, ls_remote_head, ref_commit,
    tag_commit,
};
use super::url::parse_git_protocol_url;
use crate::SourceError;
use fixtures::{FAKE_SHA, FLUSH, ref_line, ref_line_with_caps};
use gradient_types::RepositoryUrl;
use gradient_types::branch_key::NO_BRANCH;

fn default_head(reader: &mut dyn std::io::Read) -> Result<BranchHead, SourceError> {
    branch_head(
        &read_advertised_refs(reader, MAX_GIT_REF_ADVERTISEMENT_READ_BYTE_COUNT)?,
        None,
    )
}

fn advertised(name: &str, oid: u8, symref_target: Option<&str>) -> AdvertisedRef {
    AdvertisedRef {
        name: name.to_owned(),
        oid: vec![oid; 20],
        symref_target: symref_target.map(str::to_owned),
    }
}

fn head_of(refs: &[AdvertisedRef], branch: Option<&str>) -> (String, Vec<u8>) {
    let head = branch_head(refs, branch).unwrap();
    (head.branch, head.commit)
}

#[test]
fn the_default_branch_key_is_the_branch_named_by_head() {
    let refs = [
        advertised("HEAD", 2, Some("refs/heads/develop")),
        advertised("refs/heads/develop", 2, None),
        advertised("refs/heads/main", 1, None),
    ];
    assert_eq!(
        head_of(&refs, None),
        ("refs/heads/develop".to_owned(), vec![2; 20])
    );
    assert_eq!(
        head_of(&refs, Some("main")),
        ("refs/heads/main".to_owned(), vec![1; 20])
    );
}

#[test]
fn without_a_symref_the_default_branch_is_the_branch_at_the_head_commit() {
    let refs = [
        advertised("HEAD", 2, None),
        advertised("refs/heads/feature", 1, None),
        advertised("refs/heads/main", 2, None),
    ];
    assert_eq!(head_of(&refs, None).0, "refs/heads/main");
}

#[test]
fn a_detached_head_belongs_to_no_branch() {
    let refs = [
        advertised("HEAD", 2, None),
        advertised("refs/heads/main", 1, None),
    ];
    assert_eq!(head_of(&refs, None), (NO_BRANCH.to_owned(), vec![2; 20]));
}

#[test]
fn a_missing_branch_is_an_error() {
    let refs = [advertised("HEAD", 2, Some("refs/heads/main"))];
    assert!(matches!(
        branch_head(&refs, Some("feature")),
        Err(SourceError::BranchNotFound { branch }) if branch == "feature"
    ));
}

#[test]
fn a_lightweight_tag_resolves_to_the_commit_it_names() {
    let refs = [
        advertised("refs/heads/main", 1, None),
        advertised("refs/tags/v1.0", 2, None),
    ];
    assert_eq!(tag_commit(&refs, "v1.0").unwrap(), vec![2; 20]);
}

#[test]
fn an_annotated_tag_resolves_to_its_peeled_commit_not_the_tag_object() {
    let refs = [
        advertised("refs/tags/v1.0", 3, None),
        advertised("refs/tags/v1.0^{}", 2, None),
        advertised("refs/tags/v1.0.1", 4, None),
    ];
    assert_eq!(tag_commit(&refs, "v1.0").unwrap(), vec![2; 20]);
}

#[test]
fn a_missing_tag_is_an_error() {
    let refs = [
        advertised("refs/heads/v1.0", 1, None),
        advertised("refs/tags/v1.0.1", 2, None),
    ];
    assert!(matches!(
        tag_commit(&refs, "v1.0"),
        Err(SourceError::TagNotFound { tag }) if tag == "v1.0"
    ));
}

#[test]
fn a_flake_ref_prefers_the_branch_over_a_tag_of_the_same_name() {
    let refs = [
        advertised("HEAD", 1, Some("refs/heads/main")),
        advertised("refs/heads/release", 2, None),
        advertised("refs/tags/release", 3, None),
    ];
    assert_eq!(ref_commit(&refs, Some("release")).unwrap(), vec![2; 20]);
    assert_eq!(ref_commit(&refs, None).unwrap(), vec![1; 20]);
}

#[test]
fn a_flake_ref_falls_back_to_the_peeled_tag_commit() {
    let refs = [
        advertised("refs/heads/main", 1, None),
        advertised("refs/tags/v1.0", 3, None),
        advertised("refs/tags/v1.0^{}", 2, None),
    ];
    assert_eq!(ref_commit(&refs, Some("v1.0")).unwrap(), vec![2; 20]);
}

#[test]
fn a_flake_ref_accepts_a_full_ref_name() {
    let refs = [
        advertised("refs/heads/main", 1, None),
        advertised("refs/heads/staging", 2, None),
    ];
    assert_eq!(
        ref_commit(&refs, Some("refs/heads/staging")).unwrap(),
        vec![2; 20]
    );
    assert!(ref_commit(&refs, Some("missing")).is_err());
}

#[test]
fn the_symref_capability_names_the_default_branch_of_a_git_daemon() {
    let mut buf = Vec::new();
    buf.extend_from_slice(&ref_line_with_caps(
        FAKE_SHA,
        "HEAD",
        "multi_ack symref=HEAD:refs/heads/develop agent=git/2",
    ));
    buf.extend_from_slice(&ref_line(FAKE_SHA, "refs/heads/develop"));
    buf.extend_from_slice(&ref_line(FAKE_SHA, "refs/heads/main"));
    buf.extend_from_slice(FLUSH);

    let head = default_head(&mut buf.as_slice()).unwrap();
    assert_eq!(head.branch, "refs/heads/develop");
    assert_eq!(hex::encode(&head.commit), FAKE_SHA);
}

#[test]
fn parse_git_protocol_url_default_port() {
    let url = parse_git_protocol_url("git://server.example.com/repo.git").unwrap();
    assert_eq!(
        (
            url.git_server_host_name(),
            url.git_server_port(),
            url.repository_path(),
        ),
        ("server.example.com", None, "repo.git")
    );
}

#[test]
fn parse_git_protocol_url_explicit_port() {
    let url = parse_git_protocol_url("git://server.example.com:9419/foo/bar.git").unwrap();
    assert_eq!(
        (
            url.git_server_host_name(),
            url.git_server_port(),
            url.repository_path(),
        ),
        ("server.example.com", Some(9419), "foo/bar.git")
    );
}

#[test]
fn parse_git_protocol_url_rejects_non_numeric_port_https_url_no_path_and_line_break() {
    for url in [
        "git://server.example.com:not-a-port/repo",
        "https://server/repo",
        "git://server.example.com",
        "git://server.example.com/repo\r\nx",
    ] {
        assert!(
            matches!(parse_git_protocol_url(url), Err(SourceError::InvalidUrl)),
            "{url:?}"
        );
    }
}

#[test]
fn read_head_from_pktlines_basic() {
    let mut buf = Vec::new();
    buf.extend_from_slice(&ref_line_with_caps(FAKE_SHA, "HEAD", "multi_ack"));
    buf.extend_from_slice(&ref_line(FAKE_SHA, "refs/heads/main"));
    buf.extend_from_slice(FLUSH);

    let result = default_head(&mut buf.as_slice()).unwrap();
    assert_eq!(hex::encode(&result.commit), FAKE_SHA);
}

#[test]
fn an_endless_ref_advertisement_stops_at_the_byte_limit() {
    let line = ref_line(FAKE_SHA, "refs/heads/main");
    let unterminated = line.repeat(1000);

    let refs = read_advertised_refs(&mut unterminated.as_slice(), line.len() * 10);

    assert!(matches!(refs, Err(SourceError::GitCommandFailed { .. })));
}

#[test]
fn read_head_from_pktlines_head_not_first() {
    let other_sha = "1111111111111111111111111111111111111111";
    let mut buf = Vec::new();
    buf.extend_from_slice(&ref_line_with_caps(other_sha, "refs/heads/main", "caps"));
    buf.extend_from_slice(&ref_line(FAKE_SHA, "HEAD"));
    buf.extend_from_slice(FLUSH);

    let result = default_head(&mut buf.as_slice()).unwrap();
    assert_eq!(hex::encode(&result.commit), FAKE_SHA);
}

#[test]
fn read_head_from_pktlines_no_head_falls_back_to_first_ref() {
    let mut buf = Vec::new();
    buf.extend_from_slice(&ref_line_with_caps(FAKE_SHA, "refs/heads/main", "caps"));
    buf.extend_from_slice(FLUSH);

    let result = default_head(&mut buf.as_slice()).unwrap();
    assert_eq!(hex::encode(&result.commit), FAKE_SHA);
}

#[test]
fn read_head_from_pktlines_empty_repo_returns_error() {
    let zero_id = "0000000000000000000000000000000000000000";
    let mut buf = Vec::new();
    buf.extend_from_slice(&ref_line_with_caps(zero_id, "capabilities^{}", "multi_ack"));
    buf.extend_from_slice(FLUSH);

    let err = default_head(&mut buf.as_slice()).unwrap_err();
    assert!(matches!(err, SourceError::EmptyRemote));
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
    let result = default_head(&mut stream).unwrap();
    let elapsed = start.elapsed();

    assert_eq!(hex::encode(&result.commit), FAKE_SHA);
    assert!(
        elapsed.as_millis() < 1000,
        "read_advertised_refs blocked for {}ms - likely still using read_to_end",
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
    let url = RepositoryUrl::local_test_repository(dir.path());

    let head = fetch_commit(
        &url,
        &GitOperationCredentials::default(),
        &head_refspec(None),
    )
    .unwrap();
    assert_eq!(head.hash, main.as_bytes());
    assert_eq!(head.message, "second\n\nbody");
    assert_eq!(head.author_name, "Ada");
    assert_eq!(head.author_email.as_deref(), Some("ada@example.com"));

    let branch = fetch_commit(
        &url,
        &GitOperationCredentials::default(),
        &head_refspec(Some("feature")),
    )
    .unwrap();
    assert_eq!(branch.hash, feature.as_bytes());
}

#[test]
fn the_default_branch_of_a_local_remote_follows_head() {
    let dir = tempfile::TempDir::new().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let main = commit_on(&repo, "main", "first");
    repo.reference("refs/heads/develop", main, true, "same commit")
        .unwrap();
    repo.set_head("refs/heads/main").unwrap();
    let url = RepositoryUrl::local_test_repository(dir.path());

    let head = ls_remote_head(&url, &GitOperationCredentials::default(), None).unwrap();
    assert_eq!(head.branch, "refs/heads/main");
    assert_eq!(head.commit, main.as_bytes());
}

#[test]
fn an_annotated_tag_on_a_local_remote_resolves_to_the_tagged_commit() {
    let dir = tempfile::TempDir::new().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    repo.set_head("refs/heads/main").unwrap();
    let tagged = commit_on(&repo, "main", "release");
    commit_on(&repo, "main", "after the release");
    let sig = git2::Signature::now("Ada", "ada@example.com").unwrap();
    let target = repo.find_object(tagged, None).unwrap();
    repo.tag("v1.0", &target, &sig, "v1.0", false).unwrap();
    let url = RepositoryUrl::local_test_repository(dir.path());

    let refs = list_remote_refs(&url, &GitOperationCredentials::default()).unwrap();
    assert_eq!(tag_commit(&refs, "v1.0").unwrap(), tagged.as_bytes());
}

#[test]
fn a_pinned_commit_below_the_tip_is_fetched_by_its_hash() {
    let dir = tempfile::TempDir::new().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    repo.set_head("refs/heads/main").unwrap();
    let pinned = commit_on(&repo, "main", "pinned\n\nbody");
    commit_on(&repo, "main", "tip");
    let url = RepositoryUrl::local_test_repository(dir.path());

    let commit = fetch_commit(
        &url,
        &GitOperationCredentials::default(),
        &pinned.to_string(),
    )
    .unwrap();
    assert_eq!(commit.hash, pinned.as_bytes());
    assert_eq!(commit.message, "pinned\n\nbody");
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

fn checked_out_file(url: &RepositoryUrl, commit: git2::Oid) -> String {
    let checkout = checkout_commit(
        url,
        &commit.to_string(),
        &GitOperationCredentials::default(),
    )
    .unwrap();
    std::fs::read_to_string(checkout.path().join("file")).unwrap()
}

#[test]
fn a_commit_below_the_tip_is_checked_out_at_its_own_tree() {
    let dir = tempfile::TempDir::new().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let pinned = commit_file(&repo, "refs/heads/main", "pinned");
    commit_file(&repo, "refs/heads/main", "tip");
    let url = RepositoryUrl::local_test_repository(dir.path());

    assert_eq!(checked_out_file(&url, pinned), "pinned");
}

#[test]
fn a_commit_is_fetched_into_a_bare_repository_without_a_working_tree() {
    let dir = tempfile::TempDir::new().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let pinned = commit_file(&repo, "refs/heads/main", "pinned");
    commit_file(&repo, "refs/heads/main", "tip");
    let url = RepositoryUrl::local_test_repository(dir.path());

    let fetched = fetch_commit_into_new_bare_temporary_repository(
        &url,
        &pinned.to_string(),
        &GitOperationCredentials::default(),
    )
    .unwrap();

    assert!(fetched.repository().is_bare());
    assert!(fetched.repository().workdir().is_none());
    let tree = fetched.fetched_commit_tree().unwrap();
    let entry = tree.get(0).unwrap();
    let blob = fetched.repository().find_blob(entry.id()).unwrap();
    assert_eq!(blob.content(), b"pinned");
}

#[test]
fn a_commit_only_on_a_pull_request_ref_is_checked_out() {
    let dir = tempfile::TempDir::new().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit_file(&repo, "refs/heads/main", "main");
    let pull = commit_file(&repo, "refs/pull/1/head", "fork");
    let url = RepositoryUrl::local_test_repository(dir.path());

    assert_eq!(checked_out_file(&url, pull), "fork");
}

#[test]
fn a_commit_lands_in_the_given_directory() {
    let origin = tempfile::TempDir::new().unwrap();
    let repo = git2::Repository::init(origin.path()).unwrap();
    let sig = git2::Signature::now("Ada", "ada@example.com").unwrap();
    let mut tree = repo.treebuilder(None).unwrap();
    tree.insert("flake.nix", repo.blob(b"{ }").unwrap(), 0o100644)
        .unwrap();
    let tree = repo.find_tree(tree.write().unwrap()).unwrap();
    let sha = repo
        .commit(Some("refs/heads/main"), &sig, &sig, "flake", &tree, &[])
        .unwrap();
    let work = tempfile::TempDir::new().unwrap();
    let url = RepositoryUrl::local_test_repository(origin.path());

    checkout_commit_into(
        work.path(),
        &url,
        &sha.to_string(),
        &GitOperationCredentials::default(),
    )
    .unwrap();

    assert_eq!(
        std::fs::read_to_string(work.path().join("flake.nix")).unwrap(),
        "{ }"
    );
    let checkout = git2::Repository::open(work.path()).unwrap();
    assert_eq!(checkout.head().unwrap().peel_to_commit().unwrap().id(), sha);
}

struct GitDaemon(std::process::Child);

impl Drop for GitDaemon {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

fn start_git_daemon(base: &std::path::Path) -> (GitDaemon, RepositoryUrl) {
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
    (
        daemon,
        format!("git://127.0.0.1:{port}/repo").parse().unwrap(),
    )
}

#[test]
fn a_branch_tip_is_fetched_shallow_from_a_server_refusing_fetches_by_hash() {
    let base = tempfile::TempDir::new().unwrap();
    let repo = git2::Repository::init(base.path().join("repo")).unwrap();
    commit_file(&repo, "refs/heads/main", "first");
    let tip = commit_file(&repo, "refs/heads/main", "tip");
    let (_daemon, url) = start_git_daemon(base.path());

    let checkout =
        checkout_commit(&url, &tip.to_string(), &GitOperationCredentials::default()).unwrap();
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

#[test]
fn fetch_commit_by_hash_fails_after_operation_deadline_and_fetch_all_branches_fallback_also_fails()
{
    let base = tempfile::TempDir::new().unwrap();
    let repo = git2::Repository::init(base.path().join("repo")).unwrap();
    let pinned = commit_file(&repo, "refs/heads/main", "pinned");
    commit_file(&repo, "refs/heads/main", "tip");
    let (_daemon, url) = start_git_daemon(base.path());
    let target = tempfile::TempDir::new().unwrap();
    let expired = BlockingGitFetchOperationLimits {
        deadline: std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(1))
            .unwrap(),
        byte_limit: usize::MAX,
        finished_fetch_bytes: 0,
    };

    let fetched = run_blocking_libgit2_operation_with_git_credentials(
        &GitOperationCredentials::default(),
        expired,
        || {
            let target = git2::Repository::init_bare(target.path())?;
            Origin::new(&target, url.git_connection_url())?.fetch_commit(pinned)
        },
    );

    assert!(fetched.is_err(), "{fetched:?}");
}

#[test]
fn fetch_outside_thread_local_limit_wrapper_uses_16_gib_byte_limit_and_30_minute_deadline() {
    let limits =
        current_thread_blocking_libgit2_fetch_time_and_byte_limits_or_ordinary_fetch_limits();

    assert_eq!(limits.byte_limit, 16 * 1024 * 1024 * 1024);
    let remaining_time_until_fetch_deadline = limits.deadline - std::time::Instant::now();
    assert!(
        remaining_time_until_fetch_deadline > std::time::Duration::from_secs(29 * 60)
            && remaining_time_until_fetch_deadline <= std::time::Duration::from_secs(30 * 60),
        "{remaining_time_until_fetch_deadline:?}"
    );
}

#[test]
fn test_byte_limit_counts_bytes_across_all_fetches_of_one_operation() {
    let base = tempfile::TempDir::new().unwrap();
    let repo = git2::Repository::init(base.path().join("repo")).unwrap();
    let first = commit_file(&repo, "refs/heads/main", "first");
    let (_daemon, url) = start_git_daemon(base.path());
    let target = tempfile::TempDir::new().unwrap();

    let second_fetch = run_blocking_libgit2_operation_with_git_credentials(
        &GitOperationCredentials::default(),
        BlockingGitFetchOperationLimits::ordinary_fetch_operation_limits(),
        || {
            let target = git2::Repository::init_bare(target.path()).unwrap();
            let mut origin = Origin::new(&target, url.git_connection_url()).unwrap();
            origin.fetch_commit(first).unwrap();
            CURRENT_THREAD_BLOCKING_LIBGIT2_FETCH_LIMITS.with_borrow_mut(|limits| {
                let limits = limits.as_mut().unwrap();
                assert!(limits.finished_fetch_bytes > 0);
                limits.byte_limit = limits.finished_fetch_bytes + 1;
            });
            let second = commit_file(&repo, "refs/heads/main", "second");
            origin.fetch_commit(second)
        },
    );

    assert!(second_fetch.is_err(), "{second_fetch:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn resolve_remote_ref_saves_the_key_of_an_unknown_host() {
    use super::ssh::SshGitPrivateKeyKnownHostsAndObservations;
    use super::ssh::tests::{
        create_temp_git_repository_with_two_commits_on_main, decode, ed25519_pem,
        generate_random_ssh_host_private_key, serve_git_over_ssh,
    };

    let (dir, tip) = create_temp_git_repository_with_two_commits_on_main();
    let client_pem = ed25519_pem();
    let port = serve_git_over_ssh(
        decode(&client_pem).public_key().clone(),
        vec![generate_random_ssh_host_private_key(
            russh::keys::Algorithm::Ed25519,
        )],
    )
    .await;
    let url: gradient_types::RepositoryUrl =
        format!("ssh://git@127.0.0.1:{port}{}", dir.path().display())
            .parse()
            .unwrap();
    let db = gradient_fixtures::mock::mock_db()
        .append_exec_results([gradient_fixtures::mock::exec_ok()])
        .into_connection();
    let ssh = SshGitPrivateKeyKnownHostsAndObservations::new(client_pem.into(), Vec::new());

    let head = super::list_remote_refs_find_branch_head_and_save_host_keys(
        &db,
        gradient_types::ids::ProjectId::now_v7(),
        &url,
        GitOperationCredentials::from(Some(ssh)),
        None,
    )
    .await
    .unwrap();

    assert_eq!(head, tip.as_bytes());
    let statements = gradient_fixtures::mock::all_statements(db.into_transaction_log());
    assert!(
        statements.iter().any(|statement| statement
            .sql
            .starts_with(r#"INSERT INTO "project_ssh_host_key""#)),
        "{statements:?}"
    );
}
