// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::{
    CreateWebhookRequest, CreateWebhookResponse, DeletedResponse, DeliveryDetailWebhookDeliveryId,
    DeliveryListItemWebhookDeliveryId, RotatedSecret, UpdateWebhookRequest, WebhookResponse,
};
use crate::{Client, ConnectorError, http};
use reqwest::{Method, RequestBuilder};
use serde::de::DeserializeOwned;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebhookResource {
    Project(String),
    Team(String),
    Cache(String),
    Instance,
}

impl WebhookResource {
    fn webhooks_path_prefix(&self) -> String {
        match self {
            Self::Project(project) => format!("projects/{project}/webhooks"),
            Self::Team(team) => format!("teams/{team}/webhooks"),
            Self::Cache(cache) => format!("caches/{cache}/webhooks"),
            Self::Instance => "admin/webhooks".to_owned(),
        }
    }
}

pub struct WebhooksApi<'a>(pub(crate) &'a Client);

impl WebhooksApi<'_> {
    fn request(
        &self,
        method: Method,
        owner: &WebhookResource,
        suffix: &str,
    ) -> Result<RequestBuilder, ConnectorError> {
        http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            method,
            &format!("{}{suffix}", owner.webhooks_path_prefix()),
            true,
        )
    }

    async fn send<T: DeserializeOwned>(req: RequestBuilder) -> Result<T, ConnectorError> {
        http::decode(req.send().await?).await
    }

    pub async fn list(
        &self,
        owner: &WebhookResource,
    ) -> Result<Vec<WebhookResponse>, ConnectorError> {
        Self::send(self.request(Method::GET, owner, "")?).await
    }

    pub async fn create(
        &self,
        owner: &WebhookResource,
        body: CreateWebhookRequest,
    ) -> Result<CreateWebhookResponse, ConnectorError> {
        Self::send(self.request(Method::POST, owner, "")?.json(&body)).await
    }

    pub async fn get(
        &self,
        owner: &WebhookResource,
        id: &str,
    ) -> Result<WebhookResponse, ConnectorError> {
        Self::send(self.request(Method::GET, owner, &format!("/{id}"))?).await
    }

    pub async fn update(
        &self,
        owner: &WebhookResource,
        id: &str,
        body: UpdateWebhookRequest,
    ) -> Result<WebhookResponse, ConnectorError> {
        Self::send(
            self.request(Method::PATCH, owner, &format!("/{id}"))?
                .json(&body),
        )
        .await
    }

    pub async fn delete(
        &self,
        owner: &WebhookResource,
        id: &str,
    ) -> Result<DeletedResponse, ConnectorError> {
        Self::send(self.request(Method::DELETE, owner, &format!("/{id}"))?).await
    }

    pub async fn list_webhook_delivery_attempts(
        &self,
        owner: &WebhookResource,
        id: &str,
        limit: Option<u64>,
        offset: Option<u64>,
    ) -> Result<Vec<DeliveryListItemWebhookDeliveryId>, ConnectorError> {
        let mut req = self.request(Method::GET, owner, &format!("/{id}/deliveries"))?;
        if let Some(limit) = limit {
            req = req.query(&[("limit", limit)]);
        }
        if let Some(offset) = offset {
            req = req.query(&[("offset", offset)]);
        }
        Self::send(req).await
    }

    pub async fn get_webhook_delivery_attempt(
        &self,
        owner: &WebhookResource,
        id: &str,
        delivery_id: &str,
    ) -> Result<DeliveryDetailWebhookDeliveryId, ConnectorError> {
        Self::send(self.request(
            Method::GET,
            owner,
            &format!("/{id}/deliveries/{delivery_id}"),
        )?)
        .await
    }

    pub async fn rotate_webhook_signing_secret(
        &self,
        owner: &WebhookResource,
        id: &str,
    ) -> Result<RotatedSecret, ConnectorError> {
        Self::send(self.request(Method::POST, owner, &format!("/{id}/rotate-secret"))?).await
    }

    pub async fn send_webhook_test_ping(
        &self,
        owner: &WebhookResource,
        id: &str,
    ) -> Result<DeliveryListItemWebhookDeliveryId, ConnectorError> {
        Self::send(self.request(Method::POST, owner, &format!("/{id}/test"))?).await
    }
}
