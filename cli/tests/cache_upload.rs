/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use assert_cmd::Command;
use std::fs;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn upload_nar_file_with_narinfo_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/caches/mycache/nars"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "error": false, "message": {"store_path": "/nix/store/aa-x", "created": true}
        })))
        .mount(&server)
        .await;

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
    fs::write(
        &narinfo,
        "StorePath: /nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-x\nFileHash: sha256:a\nFileSize: 3\nNarHash: sha256:b\nNarSize: 3\nReferences: \n",
    )
    .unwrap();

    Command::cargo_bin("gradient")
        .unwrap()
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
        .success();
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
