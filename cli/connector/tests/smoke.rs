use connector::Client;

#[tokio::test]
async fn all_accessors_compile() {
    let client = Client::builder().base_url("http://localhost").token("t").build().unwrap();
    let _ = client.admin();
    let _ = client.auth();
    let _ = client.build_requests();
    let _ = client.builds();
    let _ = client.caches();
    let _ = client.commits();
    let _ = client.evals();
    let _ = client.integrations();
    let _ = client.orgs();
    let _ = client.projects();
    let _ = client.server();
    let _ = client.user();
    let _ = client.webhooks();
    let _ = client.workers();
}
