// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::{
    TaskCommitStatusPostAttempt, TaskRepositoryConnectionAndCommitStatusPatchRequest,
    TaskRepositoryResponse,
};
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

    pub async fn set_task_repository_git_host_connection_and_commit_status_reporting(
        &self,
        project: &str,
        task: &str,
        body: &TaskRepositoryConnectionAndCommitStatusPatchRequest,
    ) -> Result<TaskRepositoryResponse, ConnectorError> {
        let req = self.request(Method::PATCH, project, task, "")?.json(body);
        http::decode(req.send().await?).await
    }

    pub async fn post_pending_test_commit_status_to_last_evaluated_commit(
        &self,
        project: &str,
        task: &str,
    ) -> Result<TaskCommitStatusPostAttempt, ConnectorError> {
        let req = self.request(Method::POST, project, task, "/status-test")?;
        http::decode(req.send().await?).await
    }

    pub async fn add_project_ssh_public_key_as_read_only_deploy_key(
        &self,
        project: &str,
        task: &str,
    ) -> Result<String, ConnectorError> {
        let req = self.request(Method::POST, project, task, "/install-deploy-key")?;
        http::decode(req.send().await?).await
    }

    pub async fn create_repository_webhook(
        &self,
        project: &str,
        task: &str,
    ) -> Result<String, ConnectorError> {
        let req = self.request(Method::POST, project, task, "/install-webhook")?;
        http::decode(req.send().await?).await
    }
}
