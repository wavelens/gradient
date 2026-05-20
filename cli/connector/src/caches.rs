use crate::{Client, ConnectorError, PaginatedListResponse, http};
use reqwest::Method;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CacheResponse {
    pub id: String,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub priority: i32,
    pub local_priority: Option<i32>,
    pub active: bool,
    pub managed: bool,
    pub created_by: String,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MakeCacheRequest {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub priority: i32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PatchCacheRequest {
    pub name: Option<String>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub priority: Option<i32>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CacheStats {
    pub nar_bytes_served: Option<i64>,
    pub hits: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Upstream {
    pub id: String,
    pub display_name: String,
    pub mode: String,
    pub upstream_cache_id: Option<String>,
    pub url: Option<String>,
    pub public_key: Option<String>,
}

pub struct CachesApi<'a>(pub(crate) &'a Client);

impl CachesApi<'_> {
    pub async fn list(&self) -> Result<PaginatedListResponse, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::GET, "caches", true)?;
        http::decode(req.send().await?).await
    }

    pub async fn create(&self, body: MakeCacheRequest) -> Result<String, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::PUT, "caches", true)?
            .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn available(&self) -> Result<PaginatedListResponse, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::GET, "caches/available", true)?;
        http::decode(req.send().await?).await
    }

    pub async fn get(&self, cache: &str) -> Result<CacheResponse, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::GET, &format!("caches/{cache}"), true)?;
        http::decode(req.send().await?).await
    }

    pub async fn update(&self, cache: &str, body: PatchCacheRequest) -> Result<String, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::PATCH, &format!("caches/{cache}"), true)?
            .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn delete(&self, cache: &str) -> Result<String, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::DELETE, &format!("caches/{cache}"), true)?;
        http::decode(req.send().await?).await
    }

    pub async fn active(&self, cache: &str) -> Result<bool, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::GET, &format!("caches/{cache}/active"), true)?;
        http::decode(req.send().await?).await
    }

    pub async fn set_active(&self, cache: &str, active: bool) -> Result<String, ConnectorError> {
        let method = if active { Method::PUT } else { Method::DELETE };
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), method, &format!("caches/{cache}/active"), true)?;
        http::decode(req.send().await?).await
    }

    pub async fn public(&self, cache: &str) -> Result<bool, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::GET, &format!("caches/{cache}/public"), true)?;
        http::decode(req.send().await?).await
    }

    pub async fn set_public(&self, cache: &str, public: bool) -> Result<String, ConnectorError> {
        let method = if public { Method::PUT } else { Method::DELETE };
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), method, &format!("caches/{cache}/public"), true)?;
        http::decode(req.send().await?).await
    }

    pub async fn key(&self, cache: &str) -> Result<String, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::GET, &format!("caches/{cache}/key"), true)?;
        http::decode(req.send().await?).await
    }

    pub async fn public_key(&self, cache: &str) -> Result<String, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::GET, &format!("caches/{cache}/public-key"), true)?;
        http::decode(req.send().await?).await
    }

    pub async fn stats(&self, cache: &str) -> Result<CacheStats, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::GET, &format!("caches/{cache}/stats"), true)?;
        http::decode(req.send().await?).await
    }

    pub async fn upstreams(&self, cache: &str) -> Result<Vec<Upstream>, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::GET, &format!("caches/{cache}/upstreams"), true)?;
        http::decode(req.send().await?).await
    }

    pub async fn add_upstream(&self, cache: &str, body: serde_json::Value) -> Result<String, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::POST, &format!("caches/{cache}/upstreams"), true)?
            .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn get_upstream(&self, cache: &str, id: &str) -> Result<Upstream, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::GET, &format!("caches/{cache}/upstreams/{id}"), true)?;
        http::decode(req.send().await?).await
    }

    pub async fn update_upstream(&self, cache: &str, id: &str, body: serde_json::Value) -> Result<String, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::PATCH, &format!("caches/{cache}/upstreams/{id}"), true)?
            .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn delete_upstream(&self, cache: &str, id: &str) -> Result<String, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::DELETE, &format!("caches/{cache}/upstreams/{id}"), true)?;
        http::decode(req.send().await?).await
    }
}
