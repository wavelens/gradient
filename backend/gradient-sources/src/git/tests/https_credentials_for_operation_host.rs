/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::git::git_credentials::HttpsGitCredential;
use crate::git::https_credentials_for_operation_host::{
    bare_host_matches_stored_https_credential_host_ignoring_ascii_case,
    find_https_credential_matching_url_host,
    make_username_password_callback_for_single_login_attempt_on_matching_host,
};
use git2::CredentialType;

fn https_git_credential_for_host_with_fixed_username_and_password(
    host: &str,
) -> HttpsGitCredential {
    HttpsGitCredential {
        host: host.to_owned(),
        username: "ci".to_owned(),
        password: "token".into(),
    }
}

#[test]
fn https_credential_lookup_matches_only_exact_host_and_port() {
    let stored = [
        https_git_credential_for_host_with_fixed_username_and_password("github.com"),
        https_git_credential_for_host_with_fixed_username_and_password("git.example:8443"),
    ];
    let host_for = |url: &str| {
        find_https_credential_matching_url_host(&stored, url)
            .map(|credential| credential.host.as_str())
    };

    assert_eq!(host_for("https://github.com/o/r.git"), Some("github.com"));
    assert_eq!(host_for("https://GitHub.COM/o/r.git"), Some("github.com"));
    assert_eq!(
        host_for("git+https://user@git.example:8443/o/r?dir=nix"),
        Some("git.example:8443")
    );
    for url in [
        "http://github.com/o/r.git",
        "https://api.github.com/o/r.git",
        "https://github.com.evil.example/o/r.git",
        "https://github.com:8443/o/r.git",
        "https://git.example/o/r.git",
        "ssh://git@github.com/o/r.git",
    ] {
        assert_eq!(host_for(url), None, "{url}");
    }
}

#[test]
fn credentials_callback_returns_username_and_password_on_first_call_and_errors_on_second_call() {
    let mut callback = make_username_password_callback_for_single_login_attempt_on_matching_host(
        https_git_credential_for_host_with_fixed_username_and_password("github.com"),
    );
    let url = "https://github.com/o/r.git/info/refs?service=git-upload-pack";

    assert!(callback(url, None, CredentialType::USER_PASS_PLAINTEXT).is_ok());
    let second = callback(url, None, CredentialType::USER_PASS_PLAINTEXT)
        .err()
        .expect("the second login attempt fails");
    assert!(second.message().contains("rejected"), "{second}");
}

#[test]
fn credentials_callback_rejects_other_host_or_plain_http_on_first_call() {
    for url in [
        "https://evil.example/o/r.git/info/refs",
        "http://github.com/o/r.git/info/refs",
        "https://github.com:8443/o/r.git/info/refs",
    ] {
        let mut callback =
            make_username_password_callback_for_single_login_attempt_on_matching_host(
                https_git_credential_for_host_with_fixed_username_and_password("github.com"),
            );
        assert!(
            callback(url, None, CredentialType::USER_PASS_PLAINTEXT).is_err(),
            "{url}"
        );
    }
}

#[test]
fn username_password_callback_returns_error_when_server_does_not_offer_plaintext_username_password_authentication()
 {
    let mut callback = make_username_password_callback_for_single_login_attempt_on_matching_host(
        https_git_credential_for_host_with_fixed_username_and_password("github.com"),
    );
    let refused = callback(
        "https://github.com/o/r.git/info/refs",
        None,
        CredentialType::SSH_KEY | CredentialType::DEFAULT,
    )
    .err()
    .expect("no username and password without USER_PASS_PLAINTEXT");
    assert!(refused.message().contains("authentication"), "{refused}");
}

#[test]
fn tls_host_check_accepts_stored_credential_host_and_rejects_other_host() {
    for (credential_host, bare_host) in [
        ("github.com", "github.com"),
        ("github.com", "GitHub.COM"),
        ("git.example:8443", "git.example"),
        ("[::1]:8443", "::1"),
        ("[::1]", "[::1]"),
    ] {
        assert!(
            bare_host_matches_stored_https_credential_host_ignoring_ascii_case(
                credential_host,
                bare_host
            ),
            "{credential_host} {bare_host}"
        );
    }
    for (credential_host, bare_host) in [
        ("github.com", "evil.example"),
        ("github.com", "api.github.com"),
        ("github.com", "github.com.evil.example"),
        ("git.example:8443", "evil.example"),
        ("[::1]:8443", "::2"),
    ] {
        assert!(
            !bare_host_matches_stored_https_credential_host_ignoring_ascii_case(
                credential_host,
                bare_host
            ),
            "{credential_host} {bare_host}"
        );
    }
}
