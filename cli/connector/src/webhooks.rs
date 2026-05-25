use crate::{Client, ConnectorError, http};
use reqwest::Method;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Webhook {
    pub id: String,
    pub organization: String,
    pub name: String,
    pub url: String,
    pub events: Vec<String>,
    pub active: bool,
    pub created_by: String,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MakeWebhookRequest {
    pub name: String,
    pub url: String,
    pub secret: String,
    pub events: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PatchWebhookRequest {
    pub name: Option<String>,
    pub url: Option<String>,
    pub secret: Option<String>,
    pub events: Option<Vec<String>>,
    pub active: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WebhookDelivery {
    pub id: String,
    pub event: String,
    pub success: bool,
    pub response_status: Option<i32>,
    pub error_message: Option<String>,
    pub duration_ms: i64,
    pub delivered_at: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PaginatedDeliveries {
    pub items: Vec<WebhookDelivery>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
}

pub struct WebhooksApi<'a>(pub(crate) &'a Client);

impl WebhooksApi<'_> {
    pub async fn list(&self, org: &str) -> Result<Vec<Webhook>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("webhook/{org}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn create(
        &self,
        org: &str,
        body: MakeWebhookRequest,
    ) -> Result<Webhook, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PUT,
            &format!("webhook/{org}"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn get(&self, org: &str, webhook: &str) -> Result<Webhook, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("webhook/{org}/{webhook}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn update(
        &self,
        org: &str,
        webhook: &str,
        body: PatchWebhookRequest,
    ) -> Result<Webhook, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PATCH,
            &format!("webhook/{org}/{webhook}"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn delete(&self, org: &str, webhook: &str) -> Result<bool, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!("webhook/{org}/{webhook}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn test(&self, org: &str, webhook: &str) -> Result<bool, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("webhook/{org}/{webhook}/test"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn deliveries(
        &self,
        org: &str,
        webhook: &str,
    ) -> Result<PaginatedDeliveries, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("webhook/{org}/{webhook}/deliveries"),
            true,
        )?;
        http::decode(req.send().await?).await
    }
}
