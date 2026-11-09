// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::{
    PatchWorkerRequest, ProjectWorkerEntry, RegisterWorkerRequest, RegisterWorkerResponse,
    WorkerMetricsResponse, WorkerTestResponse,
};
use crate::{Client, ConnectorError, http};
use reqwest::Method;

pub struct WorkersApi<'a>(pub(crate) &'a Client);

impl WorkersApi<'_> {
    pub async fn list(&self, project: &str) -> Result<Vec<ProjectWorkerEntry>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("projects/{project}/workers"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn create(
        &self,
        project: &str,
        body: RegisterWorkerRequest,
    ) -> Result<RegisterWorkerResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("projects/{project}/workers"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn update(
        &self,
        project: &str,
        worker_id: &str,
        body: PatchWorkerRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PATCH,
            &format!("projects/{project}/workers/{worker_id}"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn get_worker_metrics(
        &self,
        project: &str,
        worker_id: &str,
    ) -> Result<WorkerMetricsResponse, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::GET,
                &format!("projects/{project}/workers/{worker_id}/metrics"),
                None::<&()>,
            )
            .await
    }

    pub async fn test_worker_connection_and_authorization(
        &self,
        project: &str,
        worker_id: &str,
    ) -> Result<WorkerTestResponse, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::POST,
                &format!("projects/{project}/workers/{worker_id}/test"),
                None::<&()>,
            )
            .await
    }

    pub async fn delete(&self, project: &str, worker_id: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!("projects/{project}/workers/{worker_id}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }
}
