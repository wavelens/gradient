/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use axum_test::TestServer;
use gradient_entity::cache_upstream::CacheUpstreamKind;
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_entity::{cache, cache_upstream, ids::*, user};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::rows::{cache, user};
use gradient_test_support::state::TestState;

const NAR_BODY: &str = "nar-bytes";

fn spawn_loopback_http_server_with_request_path_recorder() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
    let base_url = format!(
        "http://{}",
        listener.local_addr().expect("loopback address")
    );
    let paths = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&paths);
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut request = [0u8; 4096];
            let read = stream.read(&mut request).unwrap_or(0);
            let head = String::from_utf8_lossy(&request[..read]);
            let target = head.split(' ').nth(1).unwrap_or_default().to_owned();
            recorded
                .lock()
                .expect("recorded request paths")
                .push(target);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{NAR_BODY}",
                NAR_BODY.len()
            );
        }
    });
    (base_url, paths)
}

async fn create_pull_through_cache_test_database_with_http_upstream_and_start_web_server(
    upstream_url: String,
) -> (TestDatabase, TestServer, CacheUpstreamId) {
    let db = TestDatabase::create().await;
    let conn = db.connection();
    gradient_db::rows::insert::<user::Entity, _>(&conn, user())
        .await
        .expect("insert user");
    gradient_db::rows::insert::<cache::Entity, _>(
        &conn,
        cache::Model {
            public: true,
            pull_through: true,
            ..cache()
        },
    )
    .await
    .expect("insert cache");
    let upstream = gradient_db::rows::insert::<cache_upstream::Entity, _>(
        &conn,
        cache_upstream::Model {
            id: CacheUpstreamId::now_v7(),
            cache: cache().id,
            display_name: "upstream".into(),
            mode: CacheSubscriptionMode::ReadOnly,
            kind: CacheUpstreamKind::Http,
            url: Some(upstream_url),
            public_key: Some("upstream-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=".into()),
            active: true,
            ..Default::default()
        },
    )
    .await
    .expect("insert upstream cache");

    let server = TestState::new()
        .web_db(conn.clone())
        .worker_db(conn.clone())
        .cache_db(conn)
        .cli(|cli| cli.outbound.allowed_non_public_network_targets = vec!["127.0.0.0/8".into()])
        .server();
    (db, server, upstream.id)
}

async fn assert_upstream_nar_route_rejects_file_names_that_are_not_plain_nar_file_names_without_forwarding(
    file_names: &[&str],
) {
    let (base_url, paths) = spawn_loopback_http_server_with_request_path_recorder();
    let (db, server, upstream) =
        create_pull_through_cache_test_database_with_http_upstream_and_start_web_server(base_url)
            .await;

    for file_name in file_names {
        let response = server
            .get(&format!(
                "/cache/{}/nar/upstream/{upstream}/{file_name}",
                cache().name
            ))
            .await;
        assert_eq!(
            response.status_code(),
            StatusCode::BAD_REQUEST,
            "{file_name}"
        );
    }

    assert_eq!(
        *paths.lock().expect("recorded request paths"),
        Vec::<String>::new()
    );
    db.finish().await;
}

#[tokio::test]
async fn test_refuses_double_encoded_dot_dot_file_names_with_400_and_no_upstream_request() {
    assert_upstream_nar_route_rejects_file_names_that_are_not_plain_nar_file_names_without_forwarding(&[
        "nar/%252e%252e%252fadmin",
        "nar/%252e%252e/admin",
        "nar/x/%252E%252E/%252e%252e/admin",
        "nar/.%252e/admin",
    ])
    .await;
}

#[tokio::test]
async fn test_rejects_backslash_in_filename_without_upstream_request() {
    assert_upstream_nar_route_rejects_file_names_that_are_not_plain_nar_file_names_without_forwarding(&[
        "nar/..%5cadmin",
        "nar/x/..%5c..%5cadmin",
        "nar/%5cadmin.nar",
    ])
    .await;
}

#[tokio::test]
async fn test_real_binary_cache_nar_file_names_fetch_from_upstream_at_exact_path() {
    let (base_url, paths) = spawn_loopback_http_server_with_request_path_recorder();
    let (db, server, upstream) =
        create_pull_through_cache_test_database_with_http_upstream_and_start_web_server(base_url)
            .await;
    let nested_upstream = "nar/upstream/0198a3c4-5e6f-7a8b-9c0d-1e2f3a4b5c6d/nar/1abc.nar.zst";
    let file_names = [
        "nar/1njv6dxi9xm2blq6gq7y7kyij3q3nm4kpbgw4nfr2ldwqxvzm2c3.nar.xz",
        "nar/1njv6dxi9xm2blq6gq7y7kyij3q3nm4kpbgw4nfr2ldwqxvzm2c3.nar.zst",
        "nar/0yzhigwjl6bws649vcs2asa4lbs8hg93hyix187gc7s7a74w5h80.nar",
        "nar/brj5bb4pny8pnngq3qdymkllwql6z29j-0yzhigwjl6bws649vcs2asa4lbs8hg93hyix187gc7s7a74w5h80.nar",
        "nar/0njia.nar?hash=6713ipl",
        nested_upstream,
    ];

    for file_name in file_names {
        let response = server
            .get(&format!(
                "/cache/{}/nar/upstream/{upstream}/{file_name}",
                cache().name
            ))
            .await;
        response.assert_status_ok();
        assert_eq!(response.text(), NAR_BODY, "{file_name}");
    }

    let expected: Vec<String> = file_names.iter().map(|name| format!("/{name}")).collect();
    assert_eq!(*paths.lock().unwrap(), expected);
    db.finish().await;
}
