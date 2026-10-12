use crate::api::{
    AddUpstreamRequest, CacheResponse, CacheStatsResponse, ChunkReceived, MakeCacheRequest,
    NarDetail, NarStats, NarinfoPart, PaginatedCache, PaginatedNarSummary, PatchCacheRequest,
    PatchUpstreamRequest, UpstreamCacheItem,
};
use crate::{Client, ConnectorError, http};
use reqwest::Method;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct NarListQuery {
    pub hash: Option<String>,
    pub package: Option<String>,
    pub sort: Option<String>,
    pub order: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

pub struct CachesApi<'a>(pub(crate) &'a Client);

impl CachesApi<'_> {
    pub async fn list(&self) -> Result<PaginatedCache, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            "caches",
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn create(&self, body: MakeCacheRequest) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PUT,
            "caches",
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn available(&self, name: &str) -> Result<bool, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            "caches/available",
            true,
        )?
        .query(&[("name", name)]);
        http::decode(req.send().await?).await
    }

    pub async fn get(&self, cache: &str) -> Result<CacheResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("caches/{cache}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn update(
        &self,
        cache: &str,
        body: PatchCacheRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PATCH,
            &format!("caches/{cache}"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn delete(&self, cache: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!("caches/{cache}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn active(&self, cache: &str) -> Result<bool, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("caches/{cache}/active"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn set_active(&self, cache: &str, active: bool) -> Result<String, ConnectorError> {
        let method = if active { Method::PUT } else { Method::DELETE };
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            method,
            &format!("caches/{cache}/active"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn public(&self, cache: &str) -> Result<bool, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("caches/{cache}/public"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn set_public(&self, cache: &str, public: bool) -> Result<String, ConnectorError> {
        let method = if public { Method::PUT } else { Method::DELETE };
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            method,
            &format!("caches/{cache}/public"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn key(&self, cache: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("caches/{cache}/key"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn public_key(&self, cache: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("caches/{cache}/public-key"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn stats(&self, cache: &str) -> Result<CacheStatsResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("caches/{cache}/stats"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn upstream_caches(
        &self,
        cache: &str,
    ) -> Result<Vec<UpstreamCacheItem>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("caches/{cache}/upstream-caches"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn add_upstream_cache(
        &self,
        cache: &str,
        body: AddUpstreamRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PUT,
            &format!("caches/{cache}/upstream-caches"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn update_upstream_cache(
        &self,
        cache: &str,
        id: &str,
        body: PatchUpstreamRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PATCH,
            &format!("caches/{cache}/upstream-caches/{id}"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn delete_upstream_cache(
        &self,
        cache: &str,
        id: &str,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!("caches/{cache}/upstream-caches/{id}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn nars_list(
        &self,
        cache: &str,
        q: NarListQuery,
    ) -> Result<PaginatedNarSummary, ConnectorError> {
        let mut qs: Vec<(&str, String)> = Vec::new();
        if let Some(v) = q.hash {
            qs.push(("hash", v));
        }
        if let Some(v) = q.package {
            qs.push(("package", v));
        }
        if let Some(v) = q.sort {
            qs.push(("sort", v));
        }
        if let Some(v) = q.order {
            qs.push(("order", v));
        }
        if let Some(v) = q.page {
            qs.push(("page", v.to_string()));
        }
        if let Some(v) = q.per_page {
            qs.push(("per_page", v.to_string()));
        }
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("caches/{cache}/nars"),
            true,
        )?
        .query(&qs);
        http::decode(req.send().await?).await
    }

    pub async fn nar_show(&self, cache: &str, hash: &str) -> Result<NarDetail, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("caches/{cache}/nars/{hash}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn nars_stats(&self, cache: &str) -> Result<NarStats, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("caches/{cache}/nars/stats"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn nar_upload(
        &self,
        cache: &str,
        narinfo: NarinfoPart,
        nar_bytes: Vec<u8>,
    ) -> Result<(), ConnectorError> {
        let narinfo_json = serde_json::to_string(&narinfo).map_err(ConnectorError::Decode)?;
        let nar = bytes::Bytes::from(nar_bytes);
        let resp = http::send_upload(|| {
            let form = reqwest::multipart::Form::new()
                .text("narinfo", narinfo_json.clone())
                .part(
                    "nar",
                    reqwest::multipart::Part::stream_with_length(
                        reqwest::Body::from(nar.clone()),
                        nar.len() as u64,
                    )
                    .file_name("nar"),
                );
            Ok(http::request(
                self.0.http(),
                self.0.base_url(),
                self.0.token(),
                Method::POST,
                &format!("caches/{cache}/nars"),
                true,
            )?
            .multipart(form))
        })
        .await?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ConnectorError::Unauthorized);
        }
        if !status.is_success() {
            let bytes = resp.bytes().await?;
            let message = if let Ok(env) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                env.get("message")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
                    .unwrap_or_else(|| String::from_utf8_lossy(&bytes).into_owned())
            } else {
                String::from_utf8_lossy(&bytes).into_owned()
            };
            return Err(ConnectorError::Api { status, message });
        }
        Ok(())
    }

    /// The returned staged byte count is authoritative. The caller must set its next offset to it
    /// and resend when it did not advance.
    pub async fn nar_upload_chunk(
        &self,
        cache: &str,
        store_hash: &str,
        offset: u64,
        chunk: Vec<u8>,
    ) -> Result<u64, ConnectorError> {
        let chunk = bytes::Bytes::from(chunk);
        let resp = http::send_upload(|| {
            Ok(http::request(
                self.0.http(),
                self.0.base_url(),
                self.0.token(),
                Method::PUT,
                &format!("caches/{cache}/nars/{store_hash}/chunk?offset={offset}"),
                true,
            )?
            .header("Content-Type", "application/octet-stream")
            .body(chunk.clone()))
        })
        .await?;
        let received: ChunkReceived = http::decode(resp).await?;
        Ok(received.received.unsigned_abs())
    }

    pub async fn nar_upload_finalize(
        &self,
        cache: &str,
        store_hash: &str,
        narinfo: NarinfoPart,
    ) -> Result<(), ConnectorError> {
        let resp = http::send_upload(|| {
            Ok(http::request(
                self.0.http(),
                self.0.base_url(),
                self.0.token(),
                Method::POST,
                &format!("caches/{cache}/nars/{store_hash}/finalize"),
                true,
            )?
            .json(&narinfo))
        })
        .await?;
        let _: serde_json::Value = http::decode(resp).await?;
        Ok(())
    }

    pub async fn nar_delete(&self, cache: &str, hash: &str) -> Result<(), ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!("caches/{cache}/nars/{hash}"),
            true,
        )?;
        let resp = req.send().await?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ConnectorError::Unauthorized);
        }
        if !status.is_success() {
            let bytes = resp.bytes().await?;
            let message = if let Ok(env) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                env.get("message")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
                    .unwrap_or_else(|| String::from_utf8_lossy(&bytes).into_owned())
            } else {
                String::from_utf8_lossy(&bytes).into_owned()
            };
            return Err(ConnectorError::Api { status, message });
        }
        Ok(())
    }
}
