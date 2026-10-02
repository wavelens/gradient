/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::OnceLock;

static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

pub fn client() -> &'static reqwest::Client {
    CLIENT.get_or_init(|| {
        gradient_util::http::build_client().expect("failed to build worker HTTP client")
    })
}

/// Binary caches are using redirects to hand a GET off to their object storage. [`client`] is
/// refusing redirects and would return the empty 3xx body as a download.
pub fn download_client() -> &'static reqwest::Client {
    gradient_util::http::download_client()
}
