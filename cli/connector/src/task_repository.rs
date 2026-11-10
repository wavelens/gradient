// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::{InstallDeployKeyRequest, InstallWebhookRequest, TaskRepositoryResponse};
use crate::http::encode_url_component;
use crate::{Client, ConnectorError, http};
use reqwest::{Method, RequestBuilder};

pub struct TaskRepositoryConnectionApi<'a>(pub(crate) &'a Client);

impl TaskRepositoryConnectionApi<'_> {
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
            &format!(
                "tasks/{}/{}/repository{rest}",
                encode_url_component(project),
                encode_url_component(task)
            ),
            true,
        )
    }

    pub async fn get(
        &self,
        project: &str,
        task: &str,
    ) -> Result<TaskRepositoryResponse, ConnectorError> {
        let req = self.request(Method::GET, project, task, "")?;
        http::decode(req.send().await?).await
    }

    pub async fn add_project_ssh_public_key_as_read_only_deploy_key(
        &self,
        project: &str,
        task: &str,
        body: &InstallDeployKeyRequest,
    ) -> Result<String, ConnectorError> {
        let req = self
            .request(Method::POST, project, task, "/install-deploy-key")?
            .json(body);
        http::decode(req.send().await?).await
    }

    pub async fn create_repository_webhook(
        &self,
        project: &str,
        task: &str,
        body: &InstallWebhookRequest,
    ) -> Result<String, ConnectorError> {
        let req = self
            .request(Method::POST, project, task, "/install-webhook")?
            .json(body);
        http::decode(req.send().await?).await
    }
}
