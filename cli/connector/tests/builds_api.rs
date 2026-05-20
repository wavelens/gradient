use connector::Client;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

#[tokio::test]
async fn get_build_returns_response() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/builds/b1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
            "id": "b1", "evaluation": "e1", "status": "Succeeded",
            "derivation_path": "/nix/store/abc.drv", "architecture": "x86_64-linux",
            "output": {}, "created_at": "2024-01-01T00:00:00Z", "updated_at": "2024-01-01T00:00:00Z"
        }))))
        .mount(&server)
        .await;

    let client = Client::builder().base_url(&server.uri()).token("t").build().unwrap();
    let build = client.builds().get("b1").await.unwrap();
    assert_eq!(build.id, "b1");
    assert_eq!(build.status, "Succeeded");
}

#[tokio::test]
async fn download_file_returns_bytes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/builds/b1/download/result.tar.gz"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"binary-data".to_vec()))
        .mount(&server)
        .await;

    let client = Client::builder().base_url(&server.uri()).token("t").build().unwrap();
    let bytes = client.builds().download_file("b1", "result.tar.gz").await.unwrap();
    assert_eq!(&bytes[..], b"binary-data");
}
