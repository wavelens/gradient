use crate::api::{BuildGraph, BuildProduct, BuildWithOutputs};
use crate::{Client, ConnectorError, http};
use futures::stream::Stream;
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
            self.0.stream_http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("builds/{id}/log"),
            true,
        )?;
        http::json_lines(req.send().await?).await
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
