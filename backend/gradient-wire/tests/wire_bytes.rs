/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_wire::PROTO_VERSIONS;
use gradient_wire::codec::agreement::version_frame;
use gradient_wire::codec::{Proto, from_bytes, to_bytes};
use gradient_wire::messages::ClientMessage;
use gradient_wire::types::{
    CachedPath, EvalCachePullOutcome, GrantTarget, PresignedMultipart, QueryMode, SegmentFetch,
    SegmentSource,
};

#[derive(Proto)]
struct PresignedMultipartBeforeRedactedPartUrls {
    upload_id: String,
    part_size: u64,
    part_urls: Vec<String>,
}

#[derive(Proto)]
enum GrantTargetBeforeWireFormatChange {
    Skip,
    Passthrough { resume_offset: u64 },
    Put { url: String },
    Multipart(PresignedMultipartBeforeRedactedPartUrls),
}

#[derive(Proto)]
enum TestOnlySegmentSourceBeforeChange {
    Presigned { url: String },
    Inline { size_bytes: u64 },
}

#[derive(Proto)]
struct PreviousSegmentFetchWithStringUrl {
    key: String,
    source: TestOnlySegmentSourceBeforeChange,
}

#[derive(Proto)]
enum EvalCachePullOutcomeBeforeChange {
    Miss,
    #[proto(removed(38))]
    Presigned {
        url: String,
    },
    #[proto(removed(38))]
    Inline {
        total_bytes: u64,
        stream_token: String,
    },
    Segments {
        keys: Vec<String>,
        fetch: Vec<PreviousSegmentFetchWithStringUrl>,
        stream_token: Option<String>,
    },
}

#[derive(Proto)]
struct PreviousCachedPath {
    path: String,
    cached: bool,
    file_size: Option<u64>,
    nar_size: Option<u64>,
    url: Option<String>,
    nar_hash: Option<String>,
    file_hash: Option<String>,
    references: Option<Vec<String>>,
    signatures: Option<Vec<String>>,
    deriver: Option<String>,
    ca: Option<String>,
}

fn assert_same_bytes_and_readable_by_new_type<
    Before: Proto,
    After: Proto + PartialEq + std::fmt::Debug,
>(
    before: &Before,
    after: &After,
) {
    for version in std::iter::once(37).chain(PROTO_VERSIONS) {
        let old = to_bytes(before, version);
        assert_eq!(old, to_bytes(after, version), "protocol {version}");
        if let Ok(old) = old {
            assert_eq!(
                from_bytes::<After>(old, version).as_ref(),
                Ok(after),
                "protocol {version}"
            );
        }
    }
}

#[test]
fn redacted_presigned_s3_url_wire_messages_encode_same_bytes_at_every_supported_protocol_version() {
    let url = "https://s3.example.com/bucket/key?X-Amz-Credential=AKIA&X-Amz-Signature=abc";
    let part_urls = || vec![url.to_owned(), format!("{url}&partNumber=2")];

    assert_same_bytes_and_readable_by_new_type(
        &GrantTargetBeforeWireFormatChange::Put {
            url: url.to_owned(),
        },
        &GrantTarget::Put { url: url.into() },
    );
    assert_same_bytes_and_readable_by_new_type(
        &GrantTargetBeforeWireFormatChange::Multipart(PresignedMultipartBeforeRedactedPartUrls {
            upload_id: "up-1".to_owned(),
            part_size: 64 << 20,
            part_urls: part_urls(),
        }),
        &GrantTarget::Multipart(PresignedMultipart {
            upload_id: "up-1".to_owned(),
            part_size: 64 << 20,
            part_urls: part_urls().into_iter().map(Into::into).collect(),
        }),
    );
    assert_same_bytes_and_readable_by_new_type(
        &EvalCachePullOutcomeBeforeChange::Presigned {
            url: url.to_owned(),
        },
        &EvalCachePullOutcome::Presigned { url: url.into() },
    );
    assert_same_bytes_and_readable_by_new_type(
        &EvalCachePullOutcomeBeforeChange::Segments {
            keys: vec!["k".to_owned()],
            fetch: vec![
                PreviousSegmentFetchWithStringUrl {
                    key: "k".to_owned(),
                    source: TestOnlySegmentSourceBeforeChange::Presigned {
                        url: url.to_owned(),
                    },
                },
                PreviousSegmentFetchWithStringUrl {
                    key: "i".to_owned(),
                    source: TestOnlySegmentSourceBeforeChange::Inline { size_bytes: 7 },
                },
            ],
            stream_token: Some("t".to_owned()),
        },
        &EvalCachePullOutcome::Segments {
            keys: vec!["k".to_owned()],
            fetch: vec![
                SegmentFetch {
                    key: "k".to_owned(),
                    source: SegmentSource::Presigned { url: url.into() },
                },
                SegmentFetch {
                    key: "i".to_owned(),
                    source: SegmentSource::Inline { size_bytes: 7 },
                },
            ],
            stream_token: Some("t".to_owned()),
        },
    );
    assert_same_bytes_and_readable_by_new_type(
        &PreviousCachedPath {
            path: "/nix/store/aaaa-pkg".to_owned(),
            cached: true,
            file_size: Some(1),
            nar_size: Some(2),
            url: Some(url.to_owned()),
            nar_hash: Some("sha256:h".to_owned()),
            file_hash: None,
            references: Some(vec!["/nix/store/bbbb-dep".to_owned()]),
            signatures: None,
            deriver: None,
            ca: None,
        },
        &CachedPath {
            path: "/nix/store/aaaa-pkg".to_owned(),
            cached: true,
            file_size: Some(1),
            nar_size: Some(2),
            url: Some(url.into()),
            nar_hash: Some("sha256:h".to_owned()),
            file_hash: None,
            references: Some(vec!["/nix/store/bbbb-dep".to_owned()]),
            signatures: None,
            deriver: None,
            ca: None,
        },
    );
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn assert_pinned(message: ClientMessage, expected: &str) {
    let version = 65;
    let bytes = to_bytes(&message, version).expect("encodes");
    assert_eq!(hex(&bytes), expected, "protocol {version}");
    assert_eq!(from_bytes::<ClientMessage>(bytes, version), Ok(message));
}

#[test]
fn a_cache_query_keeps_its_baseline_bytes() {
    assert_pinned(
        ClientMessage::CacheQuery {
            job_id: "j".into(),
            query_id: "q".into(),
            paths: vec!["/a".into(), "/b".into()],
            mode: QueryMode::Pull,
            nar_sizes: vec![None, Some(300)],
            external: true,
        },
        "13016a017102022f61022f6201020001ac0201",
    );
}

#[test]
fn worker_metrics_keep_their_baseline_bytes() {
    assert_pinned(
        ClientMessage::WorkerMetrics {
            cpu_usage_pct: 1.5,
            ram_free_mb: 128,
            disk_speed_mbps: Some(-2.0),
            upload_speed_mbps: None,
            download_speed_mbps: None,
            build_peak_ram_mb: None,
            lifetime_left_secs: None,
            exec: None,
        },
        "050000c03f800101000000c00000000000",
    );
}

#[test]
fn the_version_frame_keeps_its_bytes() {
    assert_eq!(hex(&version_frame(&(27..=27))), "475241441b001b00");
}
