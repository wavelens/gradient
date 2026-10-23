// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::ServerConfig;
use crate::{Client, ConnectorError, http};

pub struct ServerApi<'a>(pub(crate) &'a Client);

impl ServerApi<'_> {
    pub async fn get_config(&self) -> Result<ServerConfig, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            reqwest::Method::GET,
            "config",
            false,
        )?;
        http::decode(req.send().await?).await
    }
}
