/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::fixtures::spawn_loopback_tcp_listener_counting_accepted_connections;
use super::{commit_file, start_git_daemon};
use crate::SourceError;
use crate::git::checkout::fetch_commit_into_new_bare_temporary_repository;
use crate::git::git_credentials::GitOperationCredentials;
use crate::git::remote::{branch_head, list_remote_refs};
use futures::future::BoxFuture;
use gradient_types::RepositoryUrl;
use gradient_util::http::HttpClientForUserSuppliedUrls;
use gradient_util::http_validation::{
    UserUrlDestinationAllowlist, parse_and_check_allowed_outbound_url,
};
use std::net::SocketAddr;
use std::sync::atomic::Ordering;

fn credentials_with_allowlist(allowlist: UserUrlDestinationAllowlist) -> GitOperationCredentials {
    GitOperationCredentials::default()
        .with_guarded_http_client(HttpClientForUserSuppliedUrls::new(allowlist).unwrap())
}

fn resolve_every_host_to_loopback(
    _host: String,
    port: u16,
) -> BoxFuture<'static, std::io::Result<Vec<SocketAddr>>> {
    Box::pin(async move { Ok(vec![SocketAddr::from(([127, 0, 0, 1], port))]) })
}

fn refusal_reasons_of_listing_and_fetch(
    url: &RepositoryUrl,
    credentials: &GitOperationCredentials,
) -> [String; 2] {
    let listed = list_remote_refs(url, credentials).map(drop);
    let listing_reason = match listed {
        Err(SourceError::GitServerAddressRefusedByAdminAllowlist { reason }) => reason,
        result => panic!("Expected rejection from the admin allowlist, but got {result:?}."),
    };
    let fetched =
        fetch_commit_into_new_bare_temporary_repository(url, &"a".repeat(40), credentials)
            .map(drop);
    let fetch_reason = match fetched.as_ref().map_err(|error| error.downcast_ref()) {
        Err(Some(SourceError::GitServerAddressRefusedByAdminAllowlist { reason })) => {
            reason.clone()
        }
        _ => panic!(
            "Expected rejection from the admin allowlist, but got {:?}.",
            fetched
        ),
    };
    [listing_reason, fetch_reason]
}

#[test]
fn public_only_allowlist_rejects_local_git_url_listing_and_fetch() {
    let (port, accepted) = spawn_loopback_tcp_listener_counting_accepted_connections();
    let url: RepositoryUrl = format!("git://127.0.0.1:{port}/repo").parse().unwrap();
    let allowlist = UserUrlDestinationAllowlist::default();
    let http_reason =
        parse_and_check_allowed_outbound_url(&format!("http://127.0.0.1:{port}/repo"), &allowlist)
            .unwrap_err()
            .to_string();

    let reasons =
        refusal_reasons_of_listing_and_fetch(&url, &credentials_with_allowlist(allowlist));

    assert_eq!(reasons, [http_reason.clone(), http_reason]);
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        0,
        "The listener accepted the connection, but this address is not on the admin allowlist."
    );
}

#[test]
fn list_refs_and_fetch_for_git_url_not_in_allowlist_refuse_without_address_and_accept_no_connection()
 {
    let (port, accepted) = spawn_loopback_tcp_listener_counting_accepted_connections();
    let url: RepositoryUrl = format!("git://internal.example:{port}/repo")
        .parse()
        .unwrap();
    let allowlist = UserUrlDestinationAllowlist::default()
        .with_dns_lookup_function(resolve_every_host_to_loopback);

    let reasons =
        refusal_reasons_of_listing_and_fetch(&url, &credentials_with_allowlist(allowlist));

    for reason in reasons {
        assert!(reason.contains("internal.example"), "{reason}");
        assert!(
            !reason.contains("127.0.0.1"),
            "Don't specify the address that the hostname resolved to in a rejection for a hostname: {reason}"
        );
    }
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        0,
        "The listener accepted the connection, but this address is not on the admin allowlist."
    );
}

#[test]
fn list_refs_from_loopback_git_daemon_through_libgit2_git_subtransport_head_points_to_default_branch()
 {
    let base = tempfile::TempDir::new().unwrap();
    let repo = git2::Repository::init(base.path().join("repo")).unwrap();
    repo.set_head("refs/heads/develop").unwrap();
    commit_file(&repo, "refs/heads/main", "main");
    let tip = commit_file(&repo, "refs/heads/develop", "develop");
    let (_daemon, url) = start_git_daemon(base.path());

    let refs = list_remote_refs(&url, &GitOperationCredentials::default()).unwrap();

    let head = branch_head(&refs, None).unwrap();
    assert_eq!(head.branch, "refs/heads/develop");
    assert_eq!(head.commit, tip.as_bytes());
}
