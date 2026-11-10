// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::{
    ArtefactTree, ClosureGraph, EvaluationFailureSummary, EvaluationMessageResponse,
    EvaluationResponse, FlakeGraphNode, MakeEvaluationRequest, OutputArtefacts, PaginatedBuilds,
};
use crate::http::encode_url_component;
use crate::{Client, ConnectorError, DownloadedBinaryFile, LiveUpdateEvent, http};
use futures::stream::Stream;
use reqwest::Method;
use serde::Serialize;

impl OutputArtefacts {
    /// The API is returning the prefix-free base form. An already-prefixed value is tolerated
    /// during the transition.
    pub fn full_store_path(&self) -> String {
        if self.store_path.starts_with("/nix/store/") {
            self.store_path.clone()
        } else {
            format!("/nix/store/{}", self.store_path)
        }
    }
}

pub struct EvalsApi<'a>(pub(crate) &'a Client);

impl EvalsApi<'_> {
    pub async fn get(&self, id: &str) -> Result<EvaluationResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("evals/{}", encode_url_component(id)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn abort(&self, id: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("evals/{}", encode_url_component(id)),
            true,
        )?
        .json(&MakeEvaluationRequest {
            method: "abort".to_owned(),
        });
        http::decode(req.send().await?).await
    }

    pub async fn builds(&self, id: &str) -> Result<PaginatedBuilds, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("evals/{}/builds", encode_url_component(id)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn stream_builds(
        &self,
        id: &str,
    ) -> Result<impl Stream<Item = Result<String, ConnectorError>>, ConnectorError> {
        let req = http::request(
            self.0.stream_http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("evals/{}/builds", encode_url_component(id)),
            true,
        )?;
        http::json_lines(req.send().await?).await
    }

    pub async fn messages(
        &self,
        id: &str,
    ) -> Result<Vec<EvaluationMessageResponse>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("evals/{}/messages", encode_url_component(id)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn artefacts(&self, id: &str) -> Result<ArtefactTree, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("evals/{}/artefacts", encode_url_component(id)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn retry(&self, id: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("evals/{}/retry", encode_url_component(id)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn prioritize(&self, id: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("evals/{}/prioritize", encode_url_component(id)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn summary(&self, id: &str) -> Result<EvaluationFailureSummary, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("evals/{}/summary", encode_url_component(id)),
            false,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn report(
        &self,
        id: &str,
        parameters: &EvaluationReportQueryParameters,
    ) -> Result<DownloadedBinaryFile, ConnectorError> {
        let req = http::request(
            self.0.stream_http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("evals/{}/report", encode_url_component(id)),
            true,
        )?
        .query(parameters);
        http::decode_downloaded_file(req.send().await?).await
    }

    pub async fn closure(&self, id: &str) -> Result<ClosureGraph, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("evals/{}/closure", encode_url_component(id)),
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
            &format!("evals/{}/runtime-closure", encode_url_component(id)),
            false,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn flake_graph(&self, id: &str) -> Result<Vec<FlakeGraphNode>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("evals/{}/flake-graph", encode_url_component(id)),
            false,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn live(
        &self,
        id: &str,
    ) -> Result<impl Stream<Item = Result<LiveUpdateEvent, ConnectorError>> + use<>, ConnectorError>
    {
        http::open_live_events_websocket_stream(
            self.0,
            &format!("evals/{}/live", encode_url_component(id)),
        )
        .await
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct EvaluationReportQueryParameters {
    pub anonymize_identities: bool,
    pub anonymize_packages: bool,
    pub include_logs: bool,
    pub include_instance: bool,
    pub include_exec: bool,
}
