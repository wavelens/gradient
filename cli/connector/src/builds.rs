// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::{BuildGraph, BuildProduct, BuildWithOutputs, ClosureGraph};
use crate::{Client, ConnectorError, DownloadedBinaryFile, LiveUpdateEvent, http};
use futures::stream::{Stream, StreamExt};
use reqwest::Method;

pub struct BuildsApi<'a>(pub(crate) &'a Client);

impl BuildsApi<'_> {
    pub async fn get(&self, id: &str) -> Result<BuildWithOutputs, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("builds/{id}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn log_stream(
        &self,
        id: &str,
    ) -> Result<impl Stream<Item = Result<String, ConnectorError>> + use<>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("builds/{id}/log"),
            true,
        )?;
        Ok(
            futures::stream::once(std::future::ready(Ok(http::decode::<String>(
                req.send().await?,
            )
            .await?)))
            .chain(
                http::json_lines(
                    http::request(
                        self.0.stream_http(),
                        self.0.base_url(),
                        self.0.token(),
                        Method::POST,
                        &format!("builds/{id}/log"),
                        true,
                    )?
                    .send()
                    .await?,
                )
                .await?,
            ),
        )
    }

    pub async fn log_lines(
        &self,
        id: &str,
        start: u64,
        end: Option<u64>,
    ) -> Result<String, ConnectorError> {
        let mut query = vec![("start".to_string(), start.to_string())];
        if let Some(e) = end {
            query.push(("end".to_string(), e.to_string()));
        }
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("builds/{id}/log/lines"),
            true,
        )?
        .query(&query);
        let res = req.send().await?;
        let status = res.status();
        if !status.is_success() {
            return Err(ConnectorError::Api {
                status,
                message: res.text().await?,
            });
        }
        Ok(res.text().await?)
    }

    pub async fn log_search(
        &self,
        id: &str,
        q: &str,
        case: bool,
    ) -> Result<impl Stream<Item = Result<serde_json::Value, ConnectorError>> + use<>, ConnectorError>
    {
        let req = http::request(
            self.0.stream_http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("builds/{id}/log/search"),
            true,
        )?
        .query(&[("q", q), ("case", if case { "true" } else { "false" })]);
        http::json_lines(req.send().await?).await
    }

    pub async fn graph(&self, id: &str) -> Result<BuildGraph, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("builds/{id}/graph"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn retry(&self, id: &str) -> Result<String, ConnectorError> {
        self.post_build_action_and_decode_message(id, "retry").await
    }

    pub async fn abort(&self, id: &str) -> Result<String, ConnectorError> {
        self.post_build_action_and_decode_message(id, "abort").await
    }

    pub async fn prioritize(&self, id: &str) -> Result<String, ConnectorError> {
        self.post_build_action_and_decode_message(id, "prioritize")
            .await
    }

    async fn post_build_action_and_decode_message(
        &self,
        id: &str,
        action: &str,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("builds/{id}/{action}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn closure(&self, id: &str) -> Result<ClosureGraph, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("builds/{id}/closure"),
            false,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn runtime_closure(&self, id: &str) -> Result<ClosureGraph, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("builds/{id}/runtime-closure"),
            false,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn sbom(&self, id: &str) -> Result<DownloadedBinaryFile, ConnectorError> {
        let req = http::request(
            self.0.stream_http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("builds/{id}/sbom"),
            false,
        )?;
        http::decode_downloaded_file(req.send().await?).await
    }

    pub async fn live(
        &self,
        id: &str,
    ) -> Result<impl Stream<Item = Result<LiveUpdateEvent, ConnectorError>> + use<>, ConnectorError>
    {
        http::open_live_events_websocket_stream(self.0, &format!("builds/{id}/live")).await
    }

    pub async fn downloads(&self, id: &str) -> Result<Vec<BuildProduct>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("builds/{id}/downloads"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn download_token(&self, id: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("builds/{id}/download-token"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn download_file(
        &self,
        build_id: &str,
        filename: &str,
    ) -> Result<bytes::Bytes, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("builds/{build_id}/download/{filename}"),
            true,
        )?;
        let res = req.send().await?;
        let status = res.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ConnectorError::Unauthorized);
        }
        if !status.is_success() {
            return Err(ConnectorError::Api {
                status,
                message: res.text().await.unwrap_or_default(),
            });
        }
        Ok(res.bytes().await?)
    }
}
