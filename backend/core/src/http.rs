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
//! single client built here.

use std::time::Duration;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

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
/// `reqwest` HTTPS, sea-orm postgres TLS). The call is idempotent — the second
/// install attempt returns `Err`, which we deliberately ignore.
pub fn init_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

fn rustls_config() -> rustls::ClientConfig {
    init_crypto_provider();
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth()
}

pub fn build_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(DEFAULT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(user_agent())
        .use_preconfigured_tls(rustls_config())
        .build()
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
