/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `XDG_CONFIG_HOME` must locate the config file on every platform (#536): the
//! native macOS strategy resolves to `~/Library/Application Support` and
//! ignores it, so every test that seeds a config through that variable - and
//! every user with an XDG-style dotfile setup - read from the wrong place.

use assert_cmd::Command;
use std::fs;
use tempfile::TempDir;

#[test]
fn config_is_written_below_xdg_config_home() {
    let home = TempDir::new().unwrap();

    Command::cargo_bin("gradient")
        .unwrap()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .args(["config", "server", "http://example.invalid"])
        .assert()
        .success();

    let config = home.path().join("gradient").join("config.toml");
    let contents = fs::read_to_string(&config).expect("config.toml below XDG_CONFIG_HOME");
    assert!(
        contents.contains("http://example.invalid"),
        "server not persisted: {contents}"
    );
}

#[test]
fn config_is_read_back_from_xdg_config_home() {
    let home = TempDir::new().unwrap();
    let dir = home.path().join("gradient");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("config.toml"),
        "Server = 'http://seeded.invalid'\n",
    )
    .unwrap();

    Command::cargo_bin("gradient")
        .unwrap()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .args(["config", "server"])
        .assert()
        .success()
        .stdout(predicates::str::contains("http://seeded.invalid"));
}
