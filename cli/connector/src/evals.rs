// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::{
    ArtefactTree, EvaluationMessageResponse, EvaluationResponse, MakeEvaluationRequest,
    OutputArtefacts, PaginatedBuilds,
};
use crate::{Client, ConnectorError, http};
use futures::stream::Stream;
use reqwest::Method;

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
            &format!("evals/{id}"),
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
            &format!("evals/{id}"),
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
            &format!("evals/{id}/builds"),
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
            &format!("evals/{id}/builds"),
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
            &format!("evals/{id}/messages"),
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
            &format!("evals/{id}/artefacts"),
            true,
        )?;
        http::decode(req.send().await?).await
    }
}
