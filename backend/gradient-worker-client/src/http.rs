/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::OnceLock;

use gradient_util::http::{DEFAULT_TIMEOUT, DOWNLOAD_MAX_REDIRECTS, idle_timed_download_builder};

static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
static DOWNLOAD_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

pub fn client() -> &'static reqwest::Client {
    CLIENT.get_or_init(|| {
        gradient_util::http::build_client().expect("failed to build worker HTTP client")
    })
}

/// Binary caches are using redirects to hand a GET off to their object storage. [`client`] is
/// refusing redirects and would return the empty 3xx body as a download.
pub fn download_client() -> &'static reqwest::Client {
    DOWNLOAD_CLIENT
        .get_or_init(|| build_download_client().expect("failed to build the download HTTP client"))
}

pub fn build_download_client() -> reqwest::Result<reqwest::Client> {
    idle_timed_download_builder(DEFAULT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(DOWNLOAD_MAX_REDIRECTS))
        .build()
}
