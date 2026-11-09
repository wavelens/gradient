// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::{
    CreateFlakeInputOverrideRequest, DeletedResponse, FlakeInputOverrideOut,
    UpdateFlakeInputOverrideRequest,
};
use crate::{Client, ConnectorError, http};
use reqwest::{Method, RequestBuilder};

pub struct TaskFlakeInputOverridesApi<'a>(pub(crate) &'a Client);

impl TaskFlakeInputOverridesApi<'_> {
    fn request(
        &self,
        method: Method,
        project: &str,
        task: &str,
        rest: &str,
    ) -> Result<RequestBuilder, ConnectorError> {
        http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            method,
            &format!("tasks/{project}/{task}/flake-inputs{rest}"),
            true,
        )
    }

    pub async fn list(
        &self,
        project: &str,
        task: &str,
    ) -> Result<Vec<FlakeInputOverrideOut>, ConnectorError> {
        let req = self.request(Method::GET, project, task, "")?;
        http::decode(req.send().await?).await
    }

    pub async fn create(
        &self,
        project: &str,
        task: &str,
        body: &CreateFlakeInputOverrideRequest,
    ) -> Result<FlakeInputOverrideOut, ConnectorError> {
        let req = self.request(Method::POST, project, task, "")?.json(body);
        http::decode(req.send().await?).await
    }

    pub async fn get(
        &self,
        project: &str,
        task: &str,
        id: &str,
    ) -> Result<FlakeInputOverrideOut, ConnectorError> {
        let req = self.request(Method::GET, project, task, &format!("/{id}"))?;
        http::decode(req.send().await?).await
    }

    pub async fn update(
        &self,
        project: &str,
        task: &str,
        id: &str,
        input_name: Option<&str>,
        url: Option<Option<&str>>,
    ) -> Result<FlakeInputOverrideOut, ConnectorError> {
        let mut body = serde_json::to_value(UpdateFlakeInputOverrideRequest {
            input_name: input_name.map(str::to_owned),
            url: None,
        })?;
        if let Some(url) = url {
            body["url"] = url.into();
        }
        let req = self
            .request(Method::PATCH, project, task, &format!("/{id}"))?
            .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn delete(
        &self,
        project: &str,
        task: &str,
        id: &str,
    ) -> Result<DeletedResponse, ConnectorError> {
        let req = self.request(Method::DELETE, project, task, &format!("/{id}"))?;
        http::decode(req.send().await?).await
    }
}
