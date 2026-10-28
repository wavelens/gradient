use connector::Client;
use connector::api::CreatePullRequestRequest;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn open_posts_the_patch_and_returns_the_url() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/tasks/infra/master/pull-requests"))
        .and(body_partial_json(serde_json::json!({
            "title": "update",
            "patch": "diff --git a/flake.lock b/flake.lock\n",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "error": false,
            "message": {
                "url": "https://git.example/acme/infra/pulls/7",
                "number": 7,
                "branch": "gradient/0000aaaa/1111bbbb",
            },
        })))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(server.uri())
        .token("t")
        .build()
        .unwrap();
    let opened = client
        .pull_requests()
        .open(
            "infra",
            "master",
            CreatePullRequestRequest {
                title: "update".into(),
                body: None,
                branch: None,
                base: None,
                patch: "diff --git a/flake.lock b/flake.lock\n".into(),
                commit: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(opened.url, "https://git.example/acme/infra/pulls/7");
    assert_eq!(opened.number, 7);
}
