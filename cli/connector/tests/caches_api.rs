use connector::Client;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

#[tokio::test]
async fn list_caches_decodes_bare_array() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/caches"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!([{
                "id": "019e49bd-4474-79d0-8c16-c71631fb6e64",
                "name": "krauterOS",
                "display_name": "krauterOS",
                "description": "",
                "active": true,
                "priority": 50,
                "local_priority": 10,
                "public_key": "4OAlLNfJx4uizm5zOrRrN4DwT6Af+Om7U0gvzRI8YDU=",
                "public": false,
                "created_by": "019e49bd-43c4-7ee2-afc8-40421c5ce6e9",
                "created_at": "2026-05-21T08:53:21.140900",
                "managed": true
            }]))),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let res = client.caches().list().await.unwrap();
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].name, "krauterOS");
}

#[tokio::test]
async fn get_cache_stats_returns_stats() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/caches/my-cache/stats"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
                "nar_bytes_served": 12345, "hits": 42
            }))),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let stats = client.caches().stats("my-cache").await.unwrap();
    assert_eq!(stats.hits, Some(42));
}
