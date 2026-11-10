// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::{
    BlobsResponse, ChunkReceived, DispatchRequest, DispatchResponse, InputOverrideBody,
    ManifestRequest, ManifestResponse, RunOptions, SourceFinalize,
};
use crate::http::encode_url_component;
use crate::{Client, ConnectorError, http};
use bytes::Bytes;
use reqwest::Method;
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE};

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
        blobs_by_hash: Vec<(String, Bytes)>,
    ) -> Result<BlobsResponse, ConnectorError> {
        let timer = self.0.transfer_no_progress_timer();
        let form = blobs_by_hash.into_iter().fold(
            reqwest::multipart::Form::new(),
            |form, (hash, blob)| {
                let length = blob.len() as u64;
                let body = timer.request_body_stream_from_bytes_recording_progress(blob);
                let part = reqwest::multipart::Part::stream_with_length(body, length)
                    .file_name(hash.clone());
                form.part(hash, part)
            },
        );
        let req = http::request(
            self.0.stream_http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("build-requests/{}/blobs", encode_url_component(session)),
            true,
        )?
        .multipart(form);
        let res = timer
            .send_request_and_timeout_when_no_progress_for_idle_limit(req)
            .await?;
        http::decode(res).await
    }

    pub async fn upload_source_chunk(
        &self,
        upload: &str,
        project: &str,
        offset: u64,
        chunk: Bytes,
    ) -> Result<u64, ConnectorError> {
        let timer = self.0.transfer_no_progress_timer();
        let req = http::request(
            self.0.stream_http(),
            self.0.base_url(),
            self.0.token(),
            Method::PUT,
            &format!(
                "build-requests/source/{}/chunk?offset={}&project={}",
                encode_url_component(upload),
                encode_url_component(offset),
                encode_url_component(project)
            ),
            true,
        )?
        .header(CONTENT_TYPE, "application/octet-stream")
        .header(CONTENT_LENGTH, chunk.len())
        .body(timer.request_body_stream_from_bytes_recording_progress(chunk));
        let res = timer
            .send_request_and_timeout_when_no_progress_for_idle_limit(req)
            .await?;
        let received: ChunkReceived = http::decode(res).await?;
        Ok(received.received.unsigned_abs())
    }

    pub async fn finalize_source(
        &self,
        upload: &str,
        project: &str,
        target: Option<&str>,
        system: Option<&str>,
        overrides: &[(String, String)],
        run: Option<RunOptions>,
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
            run,
        };
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!(
                "build-requests/source/{}/finalize?project={}",
                encode_url_component(upload),
                encode_url_component(project)
            ),
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
            &format!("build-requests/{}/dispatch", encode_url_component(session)),
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
            run: None,
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
            run: None,
        };
        assert!(serde_json::to_string(&set).unwrap().contains("nixpkgs"));
    }
}
