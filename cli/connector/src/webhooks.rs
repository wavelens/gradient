// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::{CreateWebhookRequest, DeliveryListItem, UpdateWebhookRequest, WebhookResponse};
use crate::{Client, ConnectorError, http};
use reqwest::Method;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PaginatedDeliveries {
    pub items: Vec<DeliveryListItem>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
}

pub struct WebhooksApi<'a>(pub(crate) &'a Client);

impl WebhooksApi<'_> {
    pub async fn list(&self, project: &str) -> Result<Vec<WebhookResponse>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("webhook/{project}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn create(
        &self,
        project: &str,
        body: CreateWebhookRequest,
    ) -> Result<WebhookResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PUT,
            &format!("webhook/{project}"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn get(
        &self,
        project: &str,
        webhook: &str,
    ) -> Result<WebhookResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("webhook/{project}/{webhook}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn update(
        &self,
        project: &str,
        webhook: &str,
        body: UpdateWebhookRequest,
    ) -> Result<WebhookResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PATCH,
            &format!("webhook/{project}/{webhook}"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn delete(&self, project: &str, webhook: &str) -> Result<bool, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!("webhook/{project}/{webhook}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn test(&self, project: &str, webhook: &str) -> Result<bool, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("webhook/{project}/{webhook}/test"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn deliveries(
        &self,
        project: &str,
        webhook: &str,
    ) -> Result<PaginatedDeliveries, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("webhook/{project}/{webhook}/deliveries"),
            true,
        )?;
        http::decode(req.send().await?).await
    }
}
