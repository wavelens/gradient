/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::git_credentials::{CURRENT_THREAD_GIT_CREDENTIALS, HttpsGitCredential};
use git2::cert::Cert;
use git2::{CertificateCheckStatus, Cred, CredentialType, RemoteCallbacks};
use gradient_types::https_git_url_host::https_host_from_repository_url;

pub(super) fn remote_callbacks_for_url_with_thread_https_credentials(
    url: &str,
) -> RemoteCallbacks<'static> {
    let mut callbacks = RemoteCallbacks::new();
    let credential = CURRENT_THREAD_GIT_CREDENTIALS.with_borrow(|credentials| {
        find_https_credential_matching_url_host(&credentials.https_git_credentials, url).cloned()
    });
    if let Some(credential) = credential {
        let credential_host = credential.host.clone();
        callbacks
            .credentials(
                make_username_password_callback_for_single_login_attempt_on_matching_host(
                    credential,
                ),
            )
            .certificate_check(certificate_check_callback_rejecting_other_hosts(
                credential_host,
            ));
    }
    callbacks
}

pub(super) fn find_https_credential_matching_url_host<'a>(
    credentials: &'a [HttpsGitCredential],
    url: &str,
) -> Option<&'a HttpsGitCredential> {
    let host = https_host_from_repository_url(url)?;
    credentials
        .iter()
        .find(|credential| credential.host.eq_ignore_ascii_case(&host))
}

pub(super) fn make_username_password_callback_for_single_login_attempt_on_matching_host(
    credential: HttpsGitCredential,
) -> impl FnMut(&str, Option<&str>, CredentialType) -> Result<Cred, git2::Error> {
    let mut asked = false;
    move |url, _username_from_url, allowed| {
        let host = &credential.host;
        if std::mem::replace(&mut asked, true) {
            return Err(git2::Error::from_str(&format!(
                "authentication failed: the server for {host} rejected the HTTPS credentials"
            )));
        }
        if find_https_credential_matching_url_host(std::slice::from_ref(&credential), url).is_none()
        {
            return Err(git2::Error::from_str(&format!(
                "Authentication rejected. HTTPS credentials are for {host} only, not for the URL that requested them."
            )));
        }
        if !allowed.contains(CredentialType::USER_PASS_PLAINTEXT) {
            return Err(git2::Error::from_str(&format!(
                "authentication failed: the server for {host} does not accept username and password"
            )));
        }
        Cred::userpass_plaintext(&credential.username, credential.password.expose())
    }
}

// A redirect response with an authentication challenge can make libgit2 pass stored credentials to the redirect target. The code for ref listing can't change the redirect setting. TLS connections to a host other than the credential host fail.
fn certificate_check_callback_rejecting_other_hosts(
    credential_host: String,
) -> impl FnMut(&Cert<'_>, &str) -> Result<CertificateCheckStatus, git2::Error> {
    move |_certificate, host| {
        if bare_host_matches_stored_https_credential_host_ignoring_ascii_case(
            &credential_host,
            host,
        ) {
            Ok(CertificateCheckStatus::CertificatePassthrough)
        } else {
            Err(git2::Error::from_str(&format!(
                "Refused connection to {host}: redirect to another host blocked. The stored HTTPS credentials are for {credential_host} only."
            )))
        }
    }
}

pub(super) fn bare_host_matches_stored_https_credential_host_ignoring_ascii_case(
    credential_host: &str,
    bare_host: &str,
) -> bool {
    let bare_host = bare_host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(bare_host);
    remove_port_and_ipv6_brackets_from_https_credential_host(credential_host)
        .eq_ignore_ascii_case(bare_host)
}

fn remove_port_and_ipv6_brackets_from_https_credential_host(host: &str) -> &str {
    match host.strip_prefix('[') {
        Some(bracketed) => bracketed
            .split_once(']')
            .map_or(bracketed, |(address, _)| address),
        None => host.split_once(':').map_or(host, |(name, _)| name),
    }
}
