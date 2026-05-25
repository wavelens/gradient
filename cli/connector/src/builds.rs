use crate::{Client, ConnectorError, http};
use futures::stream::{Stream, StreamExt};
use reqwest::Method;
use reqwest_streams::JsonStreamResponse;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BuildResponse {
    pub id: String,
    pub evaluation: String,
    pub status: String,
    pub derivation_path: String,
    pub architecture: String,
    pub worker: Option<String>,
    pub output: HashMap<String, String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BuildGraph {
    pub root: String,
    pub nodes: Vec<serde_json::Value>,
    pub edges: Vec<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BuildDependency {
    pub id: String,
    pub name: String,
    pub path: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BuildDownload {
    pub file_type: String,
    pub subtype: String,
    pub name: String,
    pub path: String,
    pub size: Option<i64>,
}

pub struct BuildsApi<'a>(pub(crate) &'a Client);

impl BuildsApi<'_> {
    pub async fn get(&self, id: &str) -> Result<BuildResponse, ConnectorError> {
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
    ) -> Result<impl Stream<Item = Result<String, ConnectorError>>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("builds/{id}/log"),
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

    pub async fn dependencies(&self, id: &str) -> Result<Vec<BuildDependency>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("builds/{id}/dependencies"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn downloads(&self, id: &str) -> Result<Vec<BuildDownload>, ConnectorError> {
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
