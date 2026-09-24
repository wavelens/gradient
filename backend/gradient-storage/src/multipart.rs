/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Presigned S3 multipart uploads: the server opens and closes the upload, the
//! worker PUTs every part straight to object storage.

use anyhow::{Context, Result};
use gradient_types::proto::{CompletedMultipart, PresignedMultipart};
use object_store::aws::AmazonS3;
use object_store::multipart::{MultipartStore as _, PartId};
use object_store::path::Path;
use object_store::signer::{SignedUrlOptions, Signer};
use std::time::Duration;

const MIB: u64 = 1024 * 1024;
const MIN_PART_BYTES: u64 = 64 * MIB;
const MAX_PARTS: u64 = 10_000;
const MAX_SIGV4_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
const COMPLETE_BUDGET: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Layout {
    pub part_size: u64,
    pub part_count: u64,
}

pub(crate) fn layout(nar_size: u64) -> Layout {
    let bound = compressed_bound(nar_size);
    let part_size = MIN_PART_BYTES
        .max(bound.div_ceil(MAX_PARTS))
        .next_multiple_of(MIB);
    Layout {
        part_size,
        part_count: bound.div_ceil(part_size).max(1),
    }
}

fn compressed_bound(nar_size: u64) -> u64 {
    nar_size.saturating_add(nar_size / 128).saturating_add(MIB)
}

fn part_ttl(nar_size: u64) -> Duration {
    let transfer = Duration::from_secs(
        compressed_bound(nar_size) / crate::nar::MIN_WRITE_THROUGHPUT_BYTES_PER_SEC,
    );
    (gradient_types::constants::PRESIGN_TTL + transfer).min(MAX_SIGV4_TTL)
}

pub(crate) async fn presign(
    s3: &AmazonS3,
    path: &Path,
    nar_size: u64,
) -> Result<PresignedMultipart> {
    let upload_id = s3
        .create_multipart(path)
        .await
        .context("failed to create multipart upload")?;
    let layout = layout(nar_size);
    match sign_parts(s3, path, &upload_id, layout.part_count, part_ttl(nar_size)).await {
        Ok(part_urls) => Ok(PresignedMultipart {
            upload_id,
            part_size: layout.part_size,
            part_urls,
        }),
        Err(e) => {
            abort(s3, path, &upload_id).await;
            Err(e)
        }
    }
}

pub(crate) async fn sign_parts(
    signer: &impl Signer,
    path: &Path,
    upload_id: &str,
    part_count: u64,
    ttl: Duration,
) -> Result<Vec<String>> {
    let mut urls = Vec::with_capacity(part_count as usize);
    for part_number in 1..=part_count {
        let options = SignedUrlOptions::default().with_query([
            ("partNumber", part_number.to_string()),
            ("uploadId", upload_id.to_owned()),
        ]);
        let url = signer
            .signed_url_opts(reqwest::Method::PUT, path, ttl, &options)
            .await
            .context("failed to presign multipart part")?;
        urls.push(url.to_string());
    }
    Ok(urls)
}

pub(crate) async fn complete(
    s3: &AmazonS3,
    path: &Path,
    receipt: &CompletedMultipart,
) -> Result<()> {
    anyhow::ensure!(
        !receipt.etags.is_empty(),
        "multipart upload reported no parts"
    );
    let parts = receipt
        .etags
        .iter()
        .map(|etag| PartId {
            content_id: etag.clone(),
        })
        .collect();
    crate::nar::bounded(
        s3.complete_multipart(path, &receipt.upload_id, parts),
        COMPLETE_BUDGET,
        "failed to complete multipart upload",
    )
    .await?;
    Ok(())
}

/// Best effort: an upload left open only costs storage until the bucket's
/// `AbortIncompleteMultipartUpload` lifecycle rule reaps it.
pub(crate) async fn abort(s3: &AmazonS3, path: &Path, upload_id: &str) {
    if let Err(e) = s3.abort_multipart(path, &upload_id.to_owned()).await {
        tracing::warn!(%path, upload_id, error = %e, "failed to abort multipart upload");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * MIB;

    #[test]
    fn layout_covers_the_compressed_bound_in_min_sized_parts() {
        let l = layout(GIB);
        assert_eq!(l.part_size, MIN_PART_BYTES);
        assert!(l.part_size * l.part_count >= compressed_bound(GIB));
        assert_eq!(l.part_count, 17);
    }

    #[test]
    fn layout_grows_parts_to_stay_under_the_part_cap() {
        let l = layout(2048 * GIB);
        assert!(l.part_count <= MAX_PARTS);
        assert!(l.part_size > MIN_PART_BYTES);
        assert_eq!(l.part_size % MIB, 0);
        assert!(l.part_size * l.part_count >= compressed_bound(2048 * GIB));
    }

    #[test]
    fn layout_saturates_at_the_part_cap_instead_of_wrapping() {
        let l = layout(u64::MAX);
        assert!(l.part_count <= MAX_PARTS);
        assert!(u128::from(l.part_size) * u128::from(l.part_count) >= u128::from(u64::MAX));
    }

    #[test]
    fn part_ttl_scales_with_size_and_caps_at_the_sigv4_limit() {
        assert!(part_ttl(GIB) > gradient_types::constants::PRESIGN_TTL);
        assert!(part_ttl(100 * GIB) > part_ttl(GIB));
        assert_eq!(part_ttl(1024 * 1024 * GIB), MAX_SIGV4_TTL);
    }

    fn s3(endpoint: Option<&str>) -> AmazonS3 {
        let mut builder = object_store::aws::AmazonS3Builder::new()
            .with_bucket_name("bucket")
            .with_region("us-east-1")
            .with_access_key_id("key")
            .with_secret_access_key("secret")
            .with_http_connector(crate::nar::SharedHttpConnector {
                read_timeout: std::time::Duration::from_secs(5),
                allow_http: endpoint.is_some(),
            });
        if let Some(endpoint) = endpoint {
            builder = builder.with_endpoint(endpoint).with_allow_http(true);
        }
        builder
            .build()
            .expect("an S3 client builds without a network")
    }

    #[tokio::test]
    async fn presign_opens_an_upload_and_signs_every_part() {
        use wiremock::matchers::{method, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(query_param("uploads", ""))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "<InitiateMultipartUploadResult><UploadId>up-1</UploadId></InitiateMultipartUploadResult>",
            ))
            .expect(1)
            .mount(&server)
            .await;

        let grant = presign(
            &s3(Some(&server.uri())),
            &Path::from("nars/ab/cd.nar.zst"),
            2 * GIB,
        )
        .await
        .expect("presign");
        assert_eq!(grant.upload_id, "up-1");
        assert_eq!(grant.part_size, layout(2 * GIB).part_size);
        assert_eq!(grant.part_urls.len() as u64, layout(2 * GIB).part_count);
        assert!(grant.part_urls[0].contains("partNumber=1"));
    }

    #[tokio::test]
    async fn complete_sends_the_etags_in_part_order() {
        use wiremock::matchers::{method, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(query_param("uploadId", "up-1"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "<CompleteMultipartUploadResult><ETag>\"all\"</ETag></CompleteMultipartUploadResult>",
            ))
            .expect(1)
            .mount(&server)
            .await;

        let receipt = CompletedMultipart {
            upload_id: "up-1".into(),
            etags: vec!["\"e1\"".into(), "\"e2\"".into()],
        };
        complete(
            &s3(Some(&server.uri())),
            &Path::from("nars/ab/cd.nar.zst"),
            &receipt,
        )
        .await
        .expect("complete");
        let body =
            String::from_utf8(server.received_requests().await.unwrap()[0].body.clone()).unwrap();
        let first = body.find("e1").expect("first etag");
        let second = body.find("e2").expect("second etag");
        assert!(first < second, "{body}");
        assert!(body.contains("<PartNumber>1</PartNumber>"), "{body}");
    }

    #[tokio::test]
    async fn complete_rejects_an_empty_receipt() {
        let receipt = CompletedMultipart {
            upload_id: "up-1".into(),
            etags: vec![],
        };
        assert!(
            complete(&s3(None), &Path::from("x"), &receipt)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn sign_parts_binds_part_number_and_upload_id() {
        let s3 = s3(None);
        let urls = sign_parts(
            &s3,
            &Path::from("nars/ab/cd.nar.zst"),
            "up-1",
            3,
            Duration::from_secs(60),
        )
        .await
        .expect("sign");
        assert_eq!(urls.len(), 3);
        for (i, url) in urls.iter().enumerate() {
            assert!(url.contains(&format!("partNumber={}", i + 1)), "{url}");
            assert!(url.contains("uploadId=up-1"), "{url}");
            assert!(url.contains("X-Amz-Signature="), "{url}");
        }
    }
}
