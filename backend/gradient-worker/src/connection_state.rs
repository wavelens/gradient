/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_util::http::HttpClientForUserSuppliedUrls;
use gradient_worker_client::connection::ProtoConnection;

pub struct Connected {
    pub(crate) conn: ProtoConnection,
    pub(crate) http_client_for_git_urls_limited_to_allowed_destinations:
        HttpClientForUserSuppliedUrls,
}

pub struct Disconnected;
