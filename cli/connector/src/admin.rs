use crate::api::{GithubAppManifestRequest, GithubAppManifestResponse, ManifestResult, WorkerInfo};
use crate::{Client, ConnectorError, http};
use reqwest::Method;

pub struct AdminApi<'a>(pub(crate) &'a Client);

impl AdminApi<'_> {
    pub async fn workers(&self) -> Result<Vec<WorkerInfo>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            "admin/workers",
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn github_app_manifest(
        &self,
        host: Option<&str>,
    ) -> Result<GithubAppManifestResponse, ConnectorError> {
        let body = GithubAppManifestRequest {
            host: host.map(str::to_owned),
        };
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            "admin/github-app/manifest",
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn github_app_credentials(&self) -> Result<ManifestResult, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            "admin/github-app/credentials",
            true,
        )?;
        http::decode(req.send().await?).await
    }
}
