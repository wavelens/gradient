/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Shared HTTP client construction.
//!
//! `reqwest::Client` is internally `Arc`'d and is designed to be cloned and
//! reused across the whole process. Constructing one per call leaks
//! connection pools and produces inconsistent timeout/redirect behaviour, so
//! all server-side and CLI-side outbound HTTP traffic should go through a
//! client built here. Two exist, differing only in redirect policy:
//! [`build_client`] for API calls and [`build_download_client`] for object
//! fetches.

use std::sync::OnceLock;
use std::time::Duration;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Redirect hops a binary-cache download follows before giving up.
const DOWNLOAD_MAX_REDIRECTS: usize = 5;

pub fn user_agent() -> String {
    format!(
        "Gradient/{} (+https://github.com/wavelens/gradient)",
        env!("CARGO_PKG_VERSION")
    )
}

/// Install the process-wide rustls `CryptoProvider`.
///
/// rustls 0.23 refuses to auto-pick a provider when zero or multiple are
/// enabled via crate features; any TLS handshake started before a provider is
/// installed panics. Binaries must call this **before** any code path opens a
/// TLS connection (e.g. `tokio_tungstenite::connect_async` for `wss://`,
/// `reqwest` HTTPS, sea-orm postgres TLS). The call is idempotent - the second
/// install attempt returns `Err`, which we deliberately ignore.
pub fn init_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

fn rustls_root_store() -> rustls::RootCertStore {
    let mut roots = rustls::RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    for cert in native.certs {
        let _ = roots.add(cert);
    }
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    roots
}

fn rustls_config() -> rustls::ClientConfig {
    init_crypto_provider();
    rustls::ClientConfig::builder()
        .with_root_certificates(rustls_root_store())
        .with_no_client_auth()
}

fn client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .timeout(DEFAULT_TIMEOUT)
        .user_agent(user_agent())
        .use_preconfigured_tls(rustls_config())
}

/// Client for API traffic (forges, webhooks, OIDC, upstream narinfo probes).
/// Redirects are refused: following one on an authenticated call is an SSRF
/// pivot, so a 3xx must surface as itself.
pub fn build_client() -> reqwest::Result<reqwest::Client> {
    client_builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
}

/// Client for fetching binary objects - NARs and cache blobs - from object
/// stores and third-party binary caches.
///
/// Unlike [`build_client`] this follows redirects. Attic, Cachix and every S3
/// gateway answer a NAR GET with a 3xx to their object storage, and reqwest
/// reports a non-followed 3xx as an ordinary response carrying an empty body -
/// indistinguishable from a successful zero-byte download. Object GETs carry no
/// credentials worth leaking, so the SSRF argument that keeps redirects off the
/// API client does not apply here.
pub fn build_download_client() -> reqwest::Result<reqwest::Client> {
    client_builder()
        .redirect(reqwest::redirect::Policy::limited(DOWNLOAD_MAX_REDIRECTS))
        .build()
}

/// Process-wide [`build_download_client`], built on first use. Every binary-cache
/// object fetch shares it, so neither the server (which threads its API client
/// through `ServerState`) nor the worker needs to carry a second client around.
///
/// Panics only if the builder cannot construct a client at all, which happens
/// solely on pathological TLS init failure - the same contract as the API
/// client's construction at startup.
pub fn download_client() -> &'static reqwest::Client {
    static DOWNLOAD_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    DOWNLOAD_CLIENT
        .get_or_init(|| build_download_client().expect("failed to build the download HTTP client"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_client_succeeds() {
        let _ = build_client().expect("client builds with defaults");
    }

    /// Regression test for issue #232: without an installed `CryptoProvider`,
    /// rustls panics inside `ClientConfig::builder()` when feature
    /// auto-detection fails. `init_crypto_provider` must be idempotent and
    /// must make subsequent rustls config construction succeed.
    #[test]
    fn init_crypto_provider_is_idempotent_and_enables_tls() {
        init_crypto_provider();
        init_crypto_provider();

        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let _ = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
    }

    /// Regression for #287: outbound HTTPS must honour OS-installed CAs so
    /// self-hosted Gradient instances with a self-signed CA work the same way
    /// `curl` does. `rustls_root_store` merges native certs with the bundled
    /// Mozilla baseline and degrades silently when the system store is absent.
    #[test]
    fn root_store_contains_webpki_baseline() {
        let roots = rustls_root_store();
        assert!(
            roots.len() >= webpki_roots::TLS_SERVER_ROOTS.len(),
            "root store missing webpki baseline",
        );
    }

    #[test]
    fn user_agent_includes_brand_and_contact_url() {
        let ua = user_agent();
        assert!(ua.starts_with("Gradient/"));
        assert!(ua.contains("(+https://github.com/wavelens/gradient)"));
    }

    #[test]
    fn user_agent_does_not_use_lowercase_brand() {
        assert!(!user_agent().starts_with("gradient/"));
    }
}
