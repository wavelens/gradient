use connector::Client;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok<T: serde::Serialize>(m: T) -> serde_json::Value {
    serde_json::json!({ "error": false, "message": m })
}

#[tokio::test]
async fn get_eval_returns_response() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/evals/eval-1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(serde_json::json!({
                "id": "eval-1", "task": "p1", "repository": "r", "commit": "c1",
                "wildcard": "*", "status": "Completed", "previous": null, "next": null,
                "created_at": "2024-01-01T00:00:00Z", "updated_at": "2024-01-01T00:00:00Z",
                "error": null
            }))),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let eval = client.evals().get("eval-1").await.unwrap();
    assert_eq!(eval.id, "eval-1");
}

#[tokio::test]
async fn abort_posts_the_abort_method() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/evals/eval-1"))
        .and(body_json(serde_json::json!({"method": "abort"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok("Success")))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    assert_eq!(client.evals().abort("eval-1").await.unwrap(), "Success");
}

#[tokio::test]
async fn stream_builds_outlives_the_request_timeout() {
    use futures::StreamExt;
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let _ = conn.read(&mut [0u8; 4096]).unwrap();
        let chunk = |s: &str| format!("{:x}\r\n{s}\r\n", s.len());
        let head = "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nTransfer-Encoding: chunked\r\n\r\n";
        conn.write_all(format!("{head}{}", chunk("\"first\"\n")).as_bytes())
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(600));
        conn.write_all(format!("{}0\r\n\r\n", chunk("\"second\"\n")).as_bytes())
            .unwrap();
    });

    let client = Client::builder()
        .base_url(format!("http://{addr}"))
        .token("t")
        .timeout(std::time::Duration::from_millis(200))
        .build()
        .unwrap();
    let lines: Vec<String> = client
        .evals()
        .stream_builds("eval-1")
        .await
        .unwrap()
        .map(Result::unwrap)
        .collect()
        .await;
    assert_eq!(lines, ["first", "second"]);
}
