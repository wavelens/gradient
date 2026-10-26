// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::{
    CreateIntegrationRequest, IntegrationResponse, IntegrationSummaryResponse,
    PatchIntegrationRequest,
};
use crate::{Client, ConnectorError, http};
use reqwest::Method;

pub struct IntegrationsApi<'a>(pub(crate) &'a Client);

impl IntegrationsApi<'_> {
    pub async fn list(&self, project: &str) -> Result<Vec<IntegrationResponse>, ConnectorError> {
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
        body: CreateIntegrationRequest,
    ) -> Result<IntegrationResponse, ConnectorError> {
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

    pub async fn summary(
        &self,
        project: &str,
    ) -> Result<Vec<IntegrationSummaryResponse>, ConnectorError> {
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

    pub async fn get(
        &self,
        project: &str,
        id: &str,
    ) -> Result<IntegrationResponse, ConnectorError> {
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
    ) -> Result<IntegrationResponse, ConnectorError> {
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
