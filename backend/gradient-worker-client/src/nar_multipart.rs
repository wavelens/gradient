/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Presigned multipart NAR upload: compressed parts go straight to object
//! storage while the next part is still being packed, so a NAR past S3's 5 GiB
//! single-PUT cap is never held whole in memory.

use anyhow::{Context, Result};
use bytes::Bytes;
use gradient_wire::types::{CompletedMultipart, PresignedMultipart};
use tokio::task::JoinSet;

const MAX_INFLIGHT_PARTS: usize = 2;

/// Something that takes a NAR's compressed bytes one part at a time.
pub(crate) trait PartSink {
    async fn send_part(&mut self, part: Vec<u8>) -> Result<()>;
}

pub struct PartUploader<'a> {
    grant: &'a PresignedMultipart,
    inflight: JoinSet<Result<(usize, String)>>,
    etags: Vec<Option<String>>,
}

impl<'a> PartUploader<'a> {
    pub fn new(grant: &'a PresignedMultipart) -> Self {
        Self {
            grant,
            inflight: JoinSet::new(),
            etags: Vec::with_capacity(grant.part_urls.len()),
        }
    }

    pub fn part_size(&self) -> usize {
        self.grant.part_size as usize
    }

    pub async fn finish(mut self) -> Result<CompletedMultipart> {
        while !self.inflight.is_empty() {
            self.settle_one().await?;
        }
        let etags = std::mem::take(&mut self.etags)
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .context("a multipart part finished without an ETag")?;
        Ok(CompletedMultipart {
            upload_id: self.grant.upload_id.clone(),
            etags,
        })
    }

    async fn settle_one(&mut self) -> Result<()> {
        if let Some(joined) = self.inflight.join_next().await {
            let (index, etag) = joined.context("multipart part task panicked")??;
            self.etags[index] = Some(etag);
        }
        Ok(())
    }
}

impl PartSink for PartUploader<'_> {
    async fn send_part(&mut self, part: Vec<u8>) -> Result<()> {
        let index = self.etags.len();
        let url = self.grant.part_urls.get(index).with_context(|| {
            format!(
                "compressed NAR outgrew its multipart grant of {} parts",
                self.grant.part_urls.len()
            )
        })?;
        while self.inflight.len() >= MAX_INFLIGHT_PARTS {
            self.settle_one().await?;
        }
        self.etags.push(None);
        let (url, body) = (url.clone(), Bytes::from(part));
        self.inflight
            .spawn(async move { put_part(url, body).await.map(|etag| (index, etag)) });
        Ok(())
    }
}

async fn put_part(url: String, body: Bytes) -> Result<String> {
    crate::object_put::put_object(&url, body, None)
        .await?
        .context("multipart part response carried no ETag")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn grant(base: &str, parts: usize) -> PresignedMultipart {
        PresignedMultipart {
            upload_id: "up-1".into(),
            part_size: 4,
            part_urls: (1..=parts).map(|i| format!("{base}/part{i}")).collect(),
        }
    }

    async fn etag_for(server: &MockServer, part: &str) {
        Mock::given(method("PUT"))
            .and(path(format!("/{part}")))
            .respond_with(ResponseTemplate::new(200).insert_header("ETag", format!("\"{part}\"")))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn parts_upload_in_order_and_return_their_etags() {
        let server = MockServer::start().await;
        for part in ["part1", "part2", "part3"] {
            etag_for(&server, part).await;
        }
        let grant = grant(&server.uri(), 3);
        let mut uploader = PartUploader::new(&grant);
        for part in [b"aaaa".to_vec(), b"bbbb".to_vec(), b"cc".to_vec()] {
            uploader.send_part(part).await.unwrap();
        }
        let receipt = uploader.finish().await.unwrap();
        assert_eq!(receipt.upload_id, "up-1");
        assert_eq!(receipt.etags, vec!["\"part1\"", "\"part2\"", "\"part3\""]);
        let mut bodies: Vec<(String, Vec<u8>)> = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .map(|r| (r.url.path().to_owned(), r.body))
            .collect();
        bodies.sort();
        assert_eq!(
            bodies,
            vec![
                ("/part1".to_owned(), b"aaaa".to_vec()),
                ("/part2".to_owned(), b"bbbb".to_vec()),
                ("/part3".to_owned(), b"cc".to_vec()),
            ]
        );
    }

    #[tokio::test]
    async fn a_server_error_on_a_part_is_retried() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        etag_for(&server, "part1").await;
        let grant = grant(&server.uri(), 1);
        let mut uploader = PartUploader::new(&grant);
        uploader.send_part(b"aaaa".to_vec()).await.unwrap();
        assert_eq!(uploader.finish().await.unwrap().etags, vec!["\"part1\""]);
    }

    #[tokio::test]
    async fn a_rejected_part_fails_without_retry() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(403))
            .expect(1)
            .mount(&server)
            .await;
        let grant = grant(&server.uri(), 1);
        let mut uploader = PartUploader::new(&grant);
        uploader.send_part(b"aaaa".to_vec()).await.unwrap();
        let err = uploader.finish().await.unwrap_err();
        assert!(format!("{err:#}").contains("403"), "{err:#}");
    }

    #[tokio::test]
    async fn more_parts_than_granted_is_an_error() {
        let grant = grant("http://127.0.0.1:1", 0);
        let mut uploader = PartUploader::new(&grant);
        let err = uploader.send_part(b"aaaa".to_vec()).await.unwrap_err();
        assert!(err.to_string().contains("outgrew"), "{err}");
    }
}
