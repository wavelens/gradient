/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use assert_cmd::Command;
use serde_json::json;
use std::fs;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn download_by_attr_filters_tree_and_writes_files() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/v1/evals/eval-1/artefacts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "error": false,
            "message": {
                "evaluation": "eval-1",
                "created_at": "2026-01-01T00:00:00Z",
                "entry_points": [{
                    "attr": "packages.x86_64-linux.my-app",
                    "derivation": "/nix/store/a.drv",
                    "build_id": "b1",
                    "outputs": [{
                        "name": "out",
                        "store_path": "/nix/store/a",
                        "products": [{
                            "id": "p1",
                            "type": "file",
                            "subtype": "",
                            "name": "default",
                            "path": "/p/my-app.tgz",
                            "size": 4
                        }]
                    }]
                }]
            }
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/v1/builds/b1/download/my-app.tgz"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"abcd".to_vec()))
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    let out_dir = TempDir::new().unwrap();

    let cfg_dir = home.path().join("gradient");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::write(
        cfg_dir.join("config.toml"),
        format!("Server = '{}'\nAuthToken = 'test-token'\n", server.uri()),
    )
    .unwrap();

    Command::cargo_bin("gradient")
        .unwrap()
        .env("XDG_CONFIG_HOME", home.path())
        .args([
            "--json",
            "download",
            "#packages.x86_64-linux.my-app",
            "--evaluation",
            "eval-1",
            "--out",
            out_dir.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    let contents = fs::read(out_dir.path().join("my-app.tgz")).unwrap();
    assert_eq!(contents, b"abcd");
}

#[tokio::test]
async fn download_json_without_args_exits_with_missing_argument() {
    let home = TempDir::new().unwrap();
    let cfg_dir = home.path().join("gradient");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::write(
        cfg_dir.join("config.toml"),
        "Server = 'http://localhost:0'\nAuthToken = 't'\n",
    )
    .unwrap();

    let output = Command::cargo_bin("gradient")
        .unwrap()
        .env("XDG_CONFIG_HOME", home.path())
        .args(["--json", "download"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let env: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(env["error"], true);
    assert!(
        env["message"]
            .as_str()
            .unwrap()
            .contains("missing argument")
    );
}
