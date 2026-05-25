use crate::{Client, ConnectorError, http};
use futures::stream::{Stream, StreamExt};
use reqwest::Method;
use reqwest_streams::JsonStreamResponse;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct EvaluationResponse {
    pub id: String,
    pub project: Option<String>,
    pub repository: String,
    pub commit: String,
    pub wildcard: String,
    pub status: String,
    pub previous: Option<String>,
    pub next: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BuildItem {
    pub id: String,
    pub name: String,
    pub status: String,
    pub updated_at: String,
    pub build_time_ms: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PaginatedBuilds {
    pub builds: Vec<BuildItem>,
    pub total: usize,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct EvalMessage {
    pub id: String,
    pub level: String,
    pub message: String,
    pub source: Option<String>,
    pub created_at: String,
    pub entry_points: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ArtefactTree {
    pub evaluation: String,
    pub created_at: String,
    pub entry_points: Vec<EntryPointArtefacts>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct EntryPointArtefacts {
    pub attr: String,
    pub derivation: String,
    pub build_id: String,
    pub outputs: Vec<OutputArtefacts>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct OutputArtefacts {
    pub name: String,
    pub store_path: String,
    pub products: Vec<ProductArtefact>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ProductArtefact {
    pub id: String,
    #[serde(rename = "type")]
    pub file_type: String,
    pub subtype: String,
    pub name: String,
    pub path: String,
    pub size: Option<i64>,
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

    pub async fn restart(&self, id: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("evals/{id}"),
            true,
        )?;
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
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("evals/{id}/builds"),
            true,
        )?;
        let res = req.send().await?;
        let status = res.status();
        if !status.is_success() {
            return Err(ConnectorError::Api {
                status,
                message: res.text().await?,
            });
        }
        Ok(res.json_nl_stream::<String>(1_024_000).map(|r| {
            r.map_err(|e| ConnectorError::Api {
                status: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                message: e.to_string(),
            })
        }))
    }

    pub async fn messages(&self, id: &str) -> Result<Vec<EvalMessage>, ConnectorError> {
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
