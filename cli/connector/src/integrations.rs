use crate::{Client, ConnectorError, http};
use reqwest::Method;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Integration {
    pub id: String,
    pub project: String,
    pub name: String,
    pub display_name: String,
    pub kind: String,
    pub forge_type: String,
    pub endpoint_url: Option<String>,
    pub has_secret: bool,
    pub has_access_token: bool,
    pub created_by: String,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IntegrationSummary {
    pub id: String,
    pub name: String,
    pub display_name: String,
    pub kind: String,
    pub forge_type: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MakeIntegrationRequest {
    pub name: String,
    pub display_name: Option<String>,
    pub kind: String,
    pub forge_type: String,
    pub secret: Option<String>,
    pub endpoint_url: Option<String>,
    pub access_token: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PatchIntegrationRequest {
    pub name: Option<String>,
    pub display_name: Option<String>,
    pub endpoint_url: Option<String>,
    pub secret: Option<String>,
    pub access_token: Option<String>,
}

pub struct IntegrationsApi<'a>(pub(crate) &'a Client);

impl IntegrationsApi<'_> {
    pub async fn list(&self, project: &str) -> Result<Vec<Integration>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("projects/{project}/integrations"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn create(
        &self,
        project: &str,
        body: MakeIntegrationRequest,
    ) -> Result<Integration, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PUT,
            &format!("projects/{project}/integrations"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn summary(&self, project: &str) -> Result<Vec<IntegrationSummary>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("projects/{project}/integrations/summary"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn get(&self, project: &str, id: &str) -> Result<Integration, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("projects/{project}/integrations/{id}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn update(
        &self,
        project: &str,
        id: &str,
        body: PatchIntegrationRequest,
    ) -> Result<Integration, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PATCH,
            &format!("projects/{project}/integrations/{id}"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn delete_one(&self, project: &str, id: &str) -> Result<bool, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!("projects/{project}/integrations/{id}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }
}
