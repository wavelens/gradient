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

/// Default request timeout applied to the shared client.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// User-agent string sent with every outbound request.
pub fn user_agent() -> String {
    format!("gradient/{}", env!("CARGO_PKG_VERSION"))
}

/// Build a `reqwest::Client` with the project-wide defaults: a 30-second
/// timeout, no redirect following, and a `gradient/<version>` user-agent.
///
/// Callers that need a different per-request timeout should override it on
/// the `RequestBuilder` (`.timeout(...)`) rather than constructing another
/// client.
pub fn build_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(DEFAULT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(user_agent())
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
