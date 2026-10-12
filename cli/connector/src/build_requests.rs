use crate::api::{
    BlobsResponse, ChunkReceived, DispatchRequest, DispatchResponse, InputOverrideBody,
    ManifestRequest, ManifestResponse, SourceFinalize,
};
use crate::{Client, ConnectorError, http};
use reqwest::Method;

pub struct BuildRequestsApi<'a>(pub(crate) &'a Client);

impl BuildRequestsApi<'_> {
    pub async fn submit_manifest(
        &self,
        body: ManifestRequest,
    ) -> Result<ManifestResponse, ConnectorError> {
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
        project: &str,
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
            &format!("build-requests/source?project={project}"),
            true,
        )?
        .multipart(form);
        http::decode(req.send().await?).await
    }

    pub async fn upload_source_chunk(
        &self,
        upload: &str,
        offset: u64,
        chunk: Vec<u8>,
    ) -> Result<u64, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PUT,
            &format!("build-requests/source/{upload}/chunk?offset={offset}"),
            true,
        )?
        .header("Content-Type", "application/octet-stream")
        .body(chunk);
        let received: ChunkReceived = http::decode(req.send().await?).await?;
        Ok(received.received.unsigned_abs())
    }

    pub async fn finalize_source(
        &self,
        upload: &str,
        project: &str,
        target: Option<&str>,
        system: Option<&str>,
        overrides: &[(String, String)],
    ) -> Result<DispatchResponse, ConnectorError> {
        let body = SourceFinalize {
            target: target.map(str::to_owned),
            system: system.map(str::to_owned),
            input_overrides: overrides
                .iter()
                .map(|(input_name, url)| InputOverrideBody {
                    input_name: input_name.clone(),
                    url: url.clone(),
                })
                .collect(),
        };
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("build-requests/source/{upload}/finalize?project={project}"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn start(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_overrides_skipped_when_empty_and_present_when_set() {
        let empty = SourceFinalize {
            target: None,
            system: None,
            input_overrides: vec![],
        };
        assert!(
            !serde_json::to_string(&empty)
                .unwrap()
                .contains("input_overrides")
        );
        let set = SourceFinalize {
            target: None,
            system: None,
            input_overrides: vec![InputOverrideBody {
                input_name: "nixpkgs".into(),
                url: "github:NixOS/nixpkgs".into(),
            }],
        };
        assert!(serde_json::to_string(&set).unwrap().contains("nixpkgs"));
    }
}
