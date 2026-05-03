/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Process-wide shared `reqwest::Client` for the worker.
//!
//! `reqwest::Client` is internally `Arc`'d and is meant to be reused. The
//! worker has many independent code paths that need outbound HTTP (presigned
//! NAR uploads, NAR import fetches, …); they all share this single client.

use std::sync::OnceLock;

static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// Return the shared HTTP client, building it on first use.
///
/// Panics only if the client builder cannot construct a client at all
/// (which only happens in pathological TLS-init failures).
pub fn client() -> &'static reqwest::Client {
    CLIENT.get_or_init(|| {
        gradient_core::http::build_client().expect("failed to build worker HTTP client")
    })
}
