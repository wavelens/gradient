// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use connector::Client;
use futures::StreamExt;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

#[tokio::test]
async fn listing_exec_job_outputs_sends_node_query_parameter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/exec/job1/outputs"))
        .and(query_param("node", "1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!([{
                "path": "reports/out.txt",
                "size": 3,
                "content_type": "text/plain",
                "created_at": "2026-11-09T00:00:00Z",
                "updated_at": "2026-11-09T00:00:00Z"
            }]))),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let files = client
        .exec_jobs()
        .list_output_files("job1", Some(1))
        .await
        .unwrap();
    assert_eq!(files[0].path, "reports/out.txt");
}

#[tokio::test]
async fn download_output_path_with_slash_encodes_as_one_url_segment_and_streams_bytes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/exec/job1/outputs/reports%2Fout.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"abc".to_vec()))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let mut chunks = client
        .exec_jobs()
        .download_job_output_file_from_node("job1", "reports/out.txt", None)
        .await
        .unwrap();
    let mut body = Vec::new();
    while let Some(chunk) = chunks.next().await {
        body.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(body, b"abc");
}

#[tokio::test]
async fn keep_vm_running_on_uses_post_and_off_uses_delete() {
    let server = MockServer::start().await;
    for verb in ["POST", "DELETE"] {
        Mock::given(method(verb))
            .and(path("/api/v1/exec/job1/keep-vm-running"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok("done")))
            .expect(1)
            .mount(&server)
            .await;
    }

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    client
        .exec_jobs()
        .set_vm_keep_running("job1", true)
        .await
        .unwrap();
    client
        .exec_jobs()
        .set_vm_keep_running("job1", false)
        .await
        .unwrap();
}

#[tokio::test]
async fn connector_exec_job_pull_requests_decode_test() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/exec/job1/pull-requests"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!([{
                "url": "https://github.com/o/r/pull/5",
                "branch": "gradient/fix",
                "number": 5,
                "integration": "github",
                "created_at": "2026-11-09T00:00:00Z"
            }]))),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let pull_requests = client
        .exec_jobs()
        .get_exec_job_pull_requests("job1")
        .await
        .unwrap();
    assert_eq!(pull_requests[0].number, 5);
}

#[tokio::test]
async fn exec_job_log_sends_offset_query_parameter_and_deserializes_offset_log_and_total_bytes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/exec/job1/log"))
        .and(query_param("offset", "6"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
                "offset": 6, "log": "bye\n", "total_bytes": 10
            }))),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let exec_job_log_reply = client.exec_jobs().log("job1", 6).await.unwrap();
    assert_eq!(
        (
            exec_job_log_reply.offset,
            exec_job_log_reply.log.as_str(),
            exec_job_log_reply.total_bytes
        ),
        (6, "bye\n", 10)
    );
}
