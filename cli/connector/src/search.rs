// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::SearchHit;
use crate::{Client, ConnectorError, http};
use reqwest::Method;

pub struct SearchApi<'a>(pub(crate) &'a Client);

impl SearchApi<'_> {
    pub async fn find_by_query(
        &self,
        q: &str,
        limit: Option<u64>,
    ) -> Result<Vec<SearchHit>, ConnectorError> {
        let mut req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            "search",
            true,
        )?
        .query(&[("q", q)]);
        if let Some(limit) = limit {
            req = req.query(&[("limit", limit)]);
        }
        http::decode(req.send().await?).await
    }
}
