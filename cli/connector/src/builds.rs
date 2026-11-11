// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::{BuildGraph, BuildProduct, BuildWithOutputs, ClosureGraph};
use crate::http::encode_url_component;
use crate::{Client, ConnectorError, DownloadedBinaryFile, LiveUpdateEvent, http};
use futures::stream::{Stream, StreamExt};
use reqwest::Method;

pub struct BuildsApi<'a>(pub(crate) &'a Client);

fn attempt_query(attempt: Option<u32>) -> Vec<(&'static str, String)> {
    attempt
        .map(|attempt| ("attempt", attempt.to_string()))
        .into_iter()
        .collect()
}

impl BuildsApi<'_> {
    pub async fn get(&self, id: &str) -> Result<BuildWithOutputs, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("builds/{}", encode_url_component(id)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn get_build_log_without_color_codes(
        &self,
        id: &str,
        attempt: Option<u32>,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("builds/{id}/log"),
            true,
        )?
        .query(&[("format", "plain")])
        .query(&attempt_query(attempt));
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
            &format!("builds/{}/log", encode_url_component(id)),
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
                        &format!("builds/{}/log", encode_url_component(id)),
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
        attempt: Option<u32>,
    ) -> Result<String, ConnectorError> {
        let mut query = vec![("start".to_string(), start.to_string())];
        if let Some(e) = end {
            query.push(("end".to_string(), e.to_string()));
        }
        if let Some(attempt) = attempt {
            query.push(("attempt".to_string(), attempt.to_string()));
        }
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("builds/{}/log/lines", encode_url_component(id)),
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
        attempt: Option<u32>,
    ) -> Result<impl Stream<Item = Result<serde_json::Value, ConnectorError>> + use<>, ConnectorError>
    {
        let req = http::request(
            self.0.stream_http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("builds/{}/log/search", encode_url_component(id)),
            true,
        )?
        .query(&[("q", q), ("case", if case { "true" } else { "false" })])
        .query(&attempt_query(attempt));
        http::json_lines(req.send().await?).await
    }

    pub async fn graph(&self, id: &str) -> Result<BuildGraph, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("builds/{}/graph", encode_url_component(id)),
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
            &format!(
                "builds/{}/{}",
                encode_url_component(id),
                encode_url_component(action)
            ),
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
            &format!("builds/{}/closure", encode_url_component(id)),
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
            &format!("builds/{}/runtime-closure", encode_url_component(id)),
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
            &format!("builds/{}/sbom", encode_url_component(id)),
            false,
        )?;
        http::decode_downloaded_file(req.send().await?).await
    }

    pub async fn live(
        &self,
        id: &str,
    ) -> Result<impl Stream<Item = Result<LiveUpdateEvent, ConnectorError>> + use<>, ConnectorError>
    {
        http::open_live_events_websocket_stream(
            self.0,
            &format!("builds/{}/live", encode_url_component(id)),
        )
        .await
    }

    pub async fn downloads(&self, id: &str) -> Result<Vec<BuildProduct>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("builds/{}/downloads", encode_url_component(id)),
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
            &format!("builds/{}/download-token", encode_url_component(id)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn download_file(
        &self,
        build_id: &str,
        filename: &str,
    ) -> Result<impl Stream<Item = Result<bytes::Bytes, ConnectorError>> + use<>, ConnectorError>
    {
        let timer = self.0.transfer_no_progress_timer();
        let req = http::request(
            self.0.stream_http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!(
                "builds/{}/download/{}",
                encode_url_component(build_id),
                encode_url_component(filename)
            ),
            true,
        )?;
        let res = timer
            .send_request_and_timeout_when_no_progress_for_idle_limit(req)
            .await?;
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
        Ok(timer.response_body_byte_stream_failing_after_idle_timeout(res))
    }
}
