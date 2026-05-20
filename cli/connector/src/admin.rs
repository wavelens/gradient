use crate::{Client, ConnectorError, http};
use reqwest::Method;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AdminWorker {
    pub id: String,
    pub architectures: Vec<String>,
    pub system_features: Vec<String>,
    pub max_concurrent_builds: i32,
    pub assigned_job_count: i32,
    pub draining: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GitHubAppManifest {
    pub manifest: serde_json::Value,
    pub post_url: String,
    pub state: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GitHubAppCredentials {
    pub id: i64,
    pub slug: String,
    pub html_url: String,
    pub pem: String,
    pub webhook_secret: String,
    pub client_id: String,
    pub client_secret: String,
}

pub struct AdminApi<'a>(pub(crate) &'a Client);

impl AdminApi<'_> {
    pub async fn workers(&self) -> Result<Vec<AdminWorker>, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::GET, "admin/workers", true)?;
        http::decode(req.send().await?).await
    }

    pub async fn github_app_manifest(&self, host: Option<&str>) -> Result<GitHubAppManifest, ConnectorError> {
        let body = serde_json::json!({ "host": host });
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::POST, "admin/github-app/manifest", true)?
            .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn github_app_credentials(&self) -> Result<GitHubAppCredentials, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::GET, "admin/github-app/credentials", true)?;
        http::decode(req.send().await?).await
    }
}
