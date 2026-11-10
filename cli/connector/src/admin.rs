// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::{
    AdminStorageResponse, AdminTaskDto, AdminWorkerListEntry, DrainingResponse,
    EffectiveConfigOption, GithubAppManifestRequest, GithubAppManifestResponse, ManifestResult,
    SetDrainingRequest, StartDeepGcResponse, UpdateWorkerRequest,
};
use crate::http::encode_url_component;
use crate::{Client, ConnectorError, http};
use reqwest::Method;

pub struct AdminApi<'a>(pub(crate) &'a Client);

impl AdminApi<'_> {
    pub async fn workers(&self) -> Result<Vec<AdminWorkerListEntry>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            "admin/workers",
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn get_registered_worker_by_id(
        &self,
        worker: &str,
    ) -> Result<AdminWorkerListEntry, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::GET,
                &format!("admin/workers/{}", encode_url_component(worker)),
                None::<&()>,
            )
            .await
    }

    pub async fn set_worker_draining_enabled(
        &self,
        worker: &str,
        draining: bool,
    ) -> Result<AdminWorkerListEntry, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::PATCH,
                &format!("admin/workers/{worker}"),
                Some(&UpdateWorkerRequest {
                    draining: Some(draining),
                }),
            )
            .await
    }

    pub async fn get_admin_config_options_with_values_and_sources(
        &self,
    ) -> Result<Vec<EffectiveConfigOption>, ConnectorError> {
        self.0
            .send_authenticated_request(Method::GET, "admin/config", None::<&()>)
            .await
    }

    pub async fn get_admin_storage_disk_usage_and_gc_stats(
        &self,
    ) -> Result<AdminStorageResponse, ConnectorError> {
        self.0
            .send_authenticated_request(Method::GET, "admin/storage", None::<&()>)
            .await
    }

    pub async fn request_gc_round(&self) -> Result<(), ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            "admin/maintenance/gc",
            true,
        )?;
        http::decode_raw_string(req.send().await?).await.map(drop)
    }

    pub async fn export_state_configuration_as_text(
        &self,
        format: &str,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            "admin/state",
            true,
        )?
        .query(&[("format", format)]);
        let res = req.send().await?;
        let is_json = res
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("application/json"));
        if !is_json {
            return http::decode_raw_string(res).await;
        }
        let state: serde_json::Value = http::decode(res).await?;
        serde_json::to_string_pretty(&state).map_err(ConnectorError::Decode)
    }

    pub async fn set_draining_enabled(
        &self,
        enabled: bool,
    ) -> Result<DrainingResponse, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::POST,
                "admin/draining",
                Some(&SetDrainingRequest { enabled }),
            )
            .await
    }

    pub async fn request_deep_gc_round(&self) -> Result<StartDeepGcResponse, ConnectorError> {
        self.0
            .send_authenticated_request(Method::POST, "admin/maintenance/deep-gc", None::<&()>)
            .await
    }

    pub async fn list_admin_background_tasks(&self) -> Result<Vec<AdminTaskDto>, ConnectorError> {
        self.0
            .send_authenticated_request(Method::GET, "admin/tasks", None::<&()>)
            .await
    }

    pub async fn get_admin_task_by_id(&self, task: &str) -> Result<AdminTaskDto, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::GET,
                &format!("admin/tasks/{}", encode_url_component(task)),
                None::<&()>,
            )
            .await
    }

    pub async fn github_app_manifest(
        &self,
        host: Option<&str>,
    ) -> Result<GithubAppManifestResponse, ConnectorError> {
        let body = GithubAppManifestRequest {
            host: host.map(str::to_owned),
        };
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            "admin/github-app/manifest",
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn github_app_credentials(&self) -> Result<ManifestResult, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            "admin/github-app/credentials",
            true,
        )?;
        http::decode(req.send().await?).await
    }
}
