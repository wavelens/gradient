// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::{
    PatchWorkerRequest, ProjectWorkerEntry, RegisterWorkerRequest, WorkerMetricsResponse,
    WorkerRegistrationResponse, WorkerTestResponse,
};
use crate::http::encode_url_component;
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
            &format!("projects/{}/workers", encode_url_component(project)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn create(
        &self,
        project: &str,
        body: RegisterWorkerRequest,
    ) -> Result<WorkerRegistrationResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("projects/{}/workers", encode_url_component(project)),
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
            &format!(
                "projects/{}/workers/{}",
                encode_url_component(project),
                encode_url_component(worker_id)
            ),
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
                &format!(
                    "projects/{}/workers/{}/metrics",
                    encode_url_component(project),
                    encode_url_component(worker_id)
                ),
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
                &format!(
                    "projects/{}/workers/{}/test",
                    encode_url_component(project),
                    encode_url_component(worker_id)
                ),
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
            &format!(
                "projects/{}/workers/{}",
                encode_url_component(project),
                encode_url_component(worker_id)
            ),
            true,
        )?;
        http::decode(req.send().await?).await
    }
}
