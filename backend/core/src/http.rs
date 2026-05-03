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
    format!("gradient/{}", env!("CARGO_PKG_VERSION"))
}

fn rustls_config() -> rustls::ClientConfig {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
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

    #[test]
    fn user_agent_is_prefixed() {
        assert!(user_agent().starts_with("gradient/"));
    }
}
