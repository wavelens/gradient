use crate::{Client, ConnectorError, http};
use reqwest::Method;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WorkerLiveInfo {
    pub architectures: Vec<String>,
    pub system_features: Vec<String>,
    pub max_concurrent_builds: u32,
    pub assigned_job_count: i32,
    pub draining: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Worker {
    pub worker_id: String,
    pub display_name: String,
    pub registered_at: String,
    pub active: bool,
    pub url: Option<String>,
    pub created_by: Option<String>,
    pub enable_fetch: bool,
    pub enable_eval: bool,
    pub enable_build: bool,
    pub live: Option<WorkerLiveInfo>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MakeWorkerRequest {
    pub worker_id: String,
    pub display_name: String,
    pub url: Option<String>,
    pub token: Option<String>,
    pub enable_fetch: Option<bool>,
    pub enable_eval: Option<bool>,
    pub enable_build: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RegisterWorkerResponse {
    pub peer_id: String,
    pub token: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PatchWorkerRequest {
    pub active: Option<bool>,
    pub display_name: Option<String>,
    pub enable_fetch: Option<bool>,
    pub enable_eval: Option<bool>,
    pub enable_build: Option<bool>,
}

pub struct WorkersApi<'a>(pub(crate) &'a Client);

impl WorkersApi<'_> {
    pub async fn list(&self, org: &str) -> Result<Vec<Worker>, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::GET, &format!("orgs/{org}/workers"), true)?;
        http::decode(req.send().await?).await
    }

    pub async fn create(&self, org: &str, body: MakeWorkerRequest) -> Result<RegisterWorkerResponse, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::POST, &format!("orgs/{org}/workers"), true)?
            .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn get(&self, org: &str, worker_id: &str) -> Result<Worker, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::GET, &format!("orgs/{org}/workers/{worker_id}"), true)?;
        http::decode(req.send().await?).await
    }

    pub async fn update(&self, org: &str, worker_id: &str, body: PatchWorkerRequest) -> Result<String, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::PATCH, &format!("orgs/{org}/workers/{worker_id}"), true)?
            .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn delete(&self, org: &str, worker_id: &str) -> Result<String, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::DELETE, &format!("orgs/{org}/workers/{worker_id}"), true)?;
        http::decode(req.send().await?).await
    }
}
