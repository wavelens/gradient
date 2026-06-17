use crate::{Client, ConnectorError, http};
use reqwest::Method;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ManifestFile {
    pub path: String,
    pub hash: String,
    pub size: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BuildManifestRequest {
    pub organization: String,
    pub files: Vec<ManifestFile>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BuildSession {
    pub session: String,
    pub missing: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BlobsResponse {
    pub uploaded: usize,
    pub remaining: usize,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct DispatchRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DispatchResponse {
    pub evaluation: String,
    pub project: String,
    pub commit: String,
    pub cache: Option<String>,
}

pub struct BuildRequestsApi<'a>(pub(crate) &'a Client);

impl BuildRequestsApi<'_> {
    pub async fn submit_manifest(
        &self,
        body: BuildManifestRequest,
    ) -> Result<BuildSession, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            "build-requests/manifest",
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn upload_blobs(
        &self,
        session: &str,
        form: reqwest::multipart::Form,
    ) -> Result<BlobsResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("build-requests/{session}/blobs"),
            true,
        )?
        .multipart(form);
        http::decode(req.send().await?).await
    }

    pub async fn upload_source_nar(
        &self,
        organization: &str,
        target: Option<&str>,
        system: Option<&str>,
        nar_bytes: Vec<u8>,
    ) -> Result<DispatchResponse, ConnectorError> {
        let mut form = reqwest::multipart::Form::new().part(
            "nar",
            reqwest::multipart::Part::bytes(nar_bytes).file_name("source.nar"),
        );
        if let Some(t) = target {
            form = form.text("target", t.to_owned());
        }

        if let Some(s) = system {
            form = form.text("system", s.to_owned());
        }

        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("build-requests/source?organization={organization}"),
            true,
        )?
        .multipart(form);
        http::decode(req.send().await?).await
    }

    pub async fn dispatch(
        &self,
        session: &str,
        body: DispatchRequest,
    ) -> Result<DispatchResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("build-requests/{session}/dispatch"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }
}
