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
use serde_json::json;
use std::fs;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn download_by_attr_filters_tree_and_writes_files() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path(
            "/api/v1/evals/6d1f0c2a-3b4e-4c5d-8e9f-0a1b2c3d4e5f/artefacts",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "error": false,
            "message": {
                "evaluation": "00000000-0000-0000-0000-000000000001",
                "created_at": "2026-01-01T00:00:00",
                "entry_points": [{
                    "attr": "packages.x86_64-linux.my-app",
                    "derivation": "/nix/store/a.drv",
                    "build_id": "00000000-0000-0000-0000-0000000000b1",
                    "outputs": [{
                        "name": "out",
                        "store_path": "/nix/store/a",
                        "products": [{
                            "id": "00000000-0000-0000-0000-0000000000a1",
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
        .and(path(
            "/api/v1/builds/00000000-0000-0000-0000-0000000000b1/download/my-app.tgz",
        ))
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
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .args([
            "--json",
            "download",
            "#packages.x86_64-linux.my-app",
            "--evaluation",
            "6d1f0c2a-3b4e-4c5d-8e9f-0a1b2c3d4e5f",
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
        .env("HOME", home.path())
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
            .contains("No argument: flake ref or --products")
    );
}

const MOCK_SERVER_EVALUATION_ID: &str = "0b9b1d2e-5f4a-4c6e-9a51-7d3e2f1a0c01";
const MOCK_SERVER_BUILD_ID: &str = "0b9b1d2e-5f4a-4c6e-9a51-7d3e2f1a0c02";
const SECOND_BUILD_ID_WITH_SAME_OUTPUT_FILE_NAME: &str = "00000000-0000-0000-0000-000000000005";

struct DownloadTestFixture {
    _server: MockServer,
    home: TempDir,
    out_dir: TempDir,
}

impl DownloadTestFixture {
    async fn start_mock_server_listing_one_build_product(product_path: &str, body: &[u8]) -> Self {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/api/v1/evals/{MOCK_SERVER_EVALUATION_ID}/artefacts"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "error": false,
                "message": {
                    "evaluation": MOCK_SERVER_EVALUATION_ID,
                    "created_at": "2026-01-01T00:00:00",
                    "entry_points": [{
                        "attr": "packages.x86_64-linux.my-app",
                        "derivation": "/nix/store/a.drv",
                        "build_id": MOCK_SERVER_BUILD_ID,
                        "outputs": [{
                            "name": "out",
                            "store_path": "/nix/store/a",
                            "products": [{
                                "id": "0b9b1d2e-5f4a-4c6e-9a51-7d3e2f1a0c03",
                                "type": "file",
                                "subtype": "",
                                "name": "default",
                                "path": product_path,
                                "size": body.len()
                            }]
                        }]
                    }]
                }
            })))
            .mount(&server)
            .await;
        let file_name = product_path.rsplit('/').next().unwrap();
        Mock::given(method("GET"))
            .and(path(format!(
                "/api/v1/builds/{MOCK_SERVER_BUILD_ID}/download/{file_name}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.to_vec()))
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
        Self {
            _server: server,
            home,
            out_dir: TempDir::new().unwrap(),
        }
    }

    fn run_gradient_download_with_extra_args_and_return_output(
        &self,
        extra_args: &[&str],
    ) -> std::process::Output {
        Command::cargo_bin("gradient")
            .unwrap()
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path())
            .args([
                "--json",
                "download",
                "#packages.x86_64-linux.my-app",
                "--evaluation",
                MOCK_SERVER_EVALUATION_ID,
                "--out",
                self.out_dir.path().to_str().unwrap(),
            ])
            .args(extra_args)
            .output()
            .unwrap()
    }
}

#[tokio::test]
async fn download_fails_and_keeps_existing_file_without_replace_existing() {
    let fixture =
        DownloadTestFixture::start_mock_server_listing_one_build_product("/p/my-app.tgz", b"new")
            .await;
    let target = fixture.out_dir.path().join("my-app.tgz");
    fs::write(&target, b"old").unwrap();

    let output = fixture.run_gradient_download_with_extra_args_and_return_output(&[]);

    assert!(!output.status.success());
    assert_eq!(fs::read(&target).unwrap(), b"old");
}

#[tokio::test]
async fn download_replace_existing_replaces_existing_file() {
    let fixture =
        DownloadTestFixture::start_mock_server_listing_one_build_product("/p/my-app.tgz", b"new")
            .await;
    let target = fixture.out_dir.path().join("my-app.tgz");
    fs::write(&target, b"old").unwrap();

    let output =
        fixture.run_gradient_download_with_extra_args_and_return_output(&["--replace-existing"]);

    assert!(output.status.success(), "{output:?}");
    assert_eq!(fs::read(&target).unwrap(), b"new");
}

#[tokio::test]
async fn download_refuses_server_file_name_starting_with_dot_and_writes_nothing() {
    let fixture = DownloadTestFixture::start_mock_server_listing_one_build_product(
        "/p/.envrc",
        b"export X=1",
    )
    .await;

    let output = fixture.run_gradient_download_with_extra_args_and_return_output(&[]);

    assert!(!output.status.success());
    assert_eq!(fs::read_dir(fixture.out_dir.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn download_shared_file_name_uses_attribute_subdirectories_and_does_not_write_through_symlink_subdirectory()
 {
    let server = MockServer::start().await;
    let entry_points: Vec<_> = [
        ("a", MOCK_SERVER_BUILD_ID),
        ("b", SECOND_BUILD_ID_WITH_SAME_OUTPUT_FILE_NAME),
    ]
    .into_iter()
    .map(|(attr, build_id)| {
        json!({
            "attr": attr,
            "derivation": "/nix/store/a.drv",
            "build_id": build_id,
            "outputs": [{
                "name": "out",
                "store_path": "/nix/store/a",
                "products": [{
                    "id": "0b9b1d2e-5f4a-4c6e-9a51-7d3e2f1a0c03",
                    "type": "file",
                    "subtype": "",
                    "name": "default",
                    "path": "/p/authorized_keys",
                    "size": 3
                }]
            }]
        })
    })
    .collect();
    Mock::given(method("GET"))
        .and(path(format!(
            "/api/v1/evals/{MOCK_SERVER_EVALUATION_ID}/artefacts"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "error": false,
            "message": {
                "evaluation": MOCK_SERVER_EVALUATION_ID,
                "created_at": "2026-01-01T00:00:00",
                "entry_points": entry_points
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"key".to_vec()))
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
    let out_dir = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    std::os::unix::fs::symlink(outside.path(), out_dir.path().join("a")).unwrap();

    let output = Command::cargo_bin("gradient")
        .unwrap()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .args([
            "--json",
            "download",
            "--products",
            "1,2",
            "--evaluation",
            MOCK_SERVER_EVALUATION_ID,
        ])
        .arg("--out")
        .arg(out_dir.path())
        .output()
        .unwrap();

    assert!(!output.status.success(), "{output:?}");
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn download_replace_existing_replaces_symlink_with_file() {
    let fixture =
        DownloadTestFixture::start_mock_server_listing_one_build_product("/p/my-app.tgz", b"new")
            .await;
    let outside = TempDir::new().unwrap();
    let link_target = outside.path().join("victim");
    fs::write(&link_target, b"untouched").unwrap();
    let link = fixture.out_dir.path().join("my-app.tgz");
    std::os::unix::fs::symlink(&link_target, &link).unwrap();

    let output =
        fixture.run_gradient_download_with_extra_args_and_return_output(&["--replace-existing"]);

    assert!(output.status.success(), "{output:?}");
    assert_eq!(fs::read(&link_target).unwrap(), b"untouched");
    assert!(
        !fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(&link).unwrap(), b"new");
}

#[tokio::test]
async fn download_exec_job_output_file_uses_plain_file_name_and_rejects_dot_file_name() {
    let server = MockServer::start().await;
    for file_name in ["app.bin", ".envrc"] {
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/exec/job1/outputs/{file_name}")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"payload".to_vec()))
            .mount(&server)
            .await;
    }
    let home = TempDir::new().unwrap();
    fs::create_dir_all(home.path().join("gradient")).unwrap();
    fs::write(
        home.path().join("gradient/config.toml"),
        format!("Server = '{}'\nAuthToken = 'test-token'\n", server.uri()),
    )
    .unwrap();
    let working_directory = TempDir::new().unwrap();
    let artifact_download = |artifact_path: &str| {
        Command::cargo_bin("gradient")
            .unwrap()
            .current_dir(working_directory.path())
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path())
            .args([
                "--json",
                "exec-job",
                "artifact",
                "download",
                "job1",
                artifact_path,
            ])
            .output()
            .unwrap()
    };

    assert!(artifact_download(".envrc").status.code() != Some(0));
    assert!(!working_directory.path().join(".envrc").exists());

    assert!(artifact_download("app.bin").status.success());
    assert_eq!(
        fs::read(working_directory.path().join("app.bin")).unwrap(),
        b"payload"
    );

    fs::write(working_directory.path().join("app.bin"), b"mine").unwrap();
    assert!(!artifact_download("app.bin").status.success());
    assert_eq!(
        fs::read(working_directory.path().join("app.bin")).unwrap(),
        b"mine"
    );
}
