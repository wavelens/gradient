/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use assert_cmd::Command;
use std::fs;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const STORE_PATH: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-x";
const STORE_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[tokio::test]
async fn upload_nar_file_with_narinfo_succeeds() {
    let server = MockServer::start().await;
    mount_nar_chunk_put_and_finalize_post_mocks(&server, 1).await;

    run_cache_upload_with_optional_narinfo_compression(&server, None).success();
    server.verify().await;
}

fn run_cache_upload_with_optional_narinfo_compression(
    server: &MockServer,
    compression: Option<&str>,
) -> assert_cmd::assert::Assert {
    let home = TempDir::new().unwrap();
    let cfg_dir = home.path().join("gradient");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::write(
        cfg_dir.join("config.toml"),
        format!("Server = '{}'\nAuthToken = 'test-token'\n", server.uri()),
    )
    .unwrap();

    let work = TempDir::new().unwrap();
    let nar = work.path().join("x.nar");
    let narinfo = work.path().join("x.narinfo");
    fs::write(&nar, b"abc").unwrap();
    let compression_line = compression
        .map(|value| format!("Compression: {value}\n"))
        .unwrap_or_default();
    fs::write(
        &narinfo,
        format!("StorePath: {STORE_PATH}\n{compression_line}FileHash: sha256:a\nFileSize: 3\nNarHash: sha256:b\nNarSize: 3\nReferences: \n"),
    )
    .unwrap();

    Command::cargo_bin("gradient")
        .unwrap()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .args([
            "cache",
            "upload",
            "--nar-file",
            nar.to_str().unwrap(),
            "--narinfo",
            narinfo.to_str().unwrap(),
            "mycache",
        ])
        .assert()
}

async fn mount_nar_chunk_put_and_finalize_post_mocks(server: &MockServer, expected_requests: u64) {
    Mock::given(method("PUT"))
        .and(path(format!(
            "/api/v1/caches/mycache/nars/{STORE_HASH}/chunk"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "error": false, "message": {"received": 3}
        })))
        .expect(expected_requests)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/v1/caches/mycache/nars/{STORE_HASH}/finalize"
        )))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "error": false, "message": {"store_path": STORE_PATH, "created": true}
        })))
        .expect(expected_requests)
        .mount(server)
        .await;
}

#[tokio::test]
async fn upload_nar_file_with_compression_none_narinfo_is_refused_without_server_request() {
    let server = MockServer::start().await;
    mount_nar_chunk_put_and_finalize_post_mocks(&server, 0).await;

    run_cache_upload_with_optional_narinfo_compression(&server, Some("none"))
        .failure()
        .stderr(predicates::str::contains(
            "The narinfo has `Compression: none`. The cache upload command accepts only NAR files compressed with zstd.",
        ));
    server.verify().await;
}

#[tokio::test]
async fn upload_nar_file_with_xz_compression_narinfo_is_refused_without_server_request() {
    let server = MockServer::start().await;
    mount_nar_chunk_put_and_finalize_post_mocks(&server, 0).await;

    run_cache_upload_with_optional_narinfo_compression(&server, Some("xz"))
        .failure()
        .stderr(predicates::str::contains(
            "The narinfo has `Compression: xz`. The cache upload command accepts only NAR files compressed with zstd.",
        ));
    server.verify().await;
}

#[tokio::test]
async fn upload_nar_file_with_zstd_compression_succeeds() {
    let server = MockServer::start().await;
    mount_nar_chunk_put_and_finalize_post_mocks(&server, 1).await;

    run_cache_upload_with_optional_narinfo_compression(&server, Some("zstd")).success();
    server.verify().await;
}

#[tokio::test]
async fn upload_nar_file_without_narinfo_errors() {
    let home = TempDir::new().unwrap();
    let cfg_dir = home.path().join("gradient");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::write(
        cfg_dir.join("config.toml"),
        "Server = 'http://localhost:1'\nAuthToken = 't'\n",
    )
    .unwrap();

    let work = TempDir::new().unwrap();
    let nar = work.path().join("x.nar");
    fs::write(&nar, b"abc").unwrap();

    Command::cargo_bin("gradient")
        .unwrap()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .args([
            "cache",
            "upload",
            "--nar-file",
            nar.to_str().unwrap(),
            "mycache",
        ])
        .assert()
        .failure();
}
