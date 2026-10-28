/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use assert_cmd::Command;
use tempfile::TempDir;

#[test]
fn pr_open_without_a_selected_task_fails_with_a_hint() {
    let home = TempDir::new().unwrap();

    Command::cargo_bin("gradient")
        .unwrap()
        .current_dir(home.path())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env_remove("GRADIENT_JOB")
        .args(["pr", "open", "--title", "update"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("gradient task select"));
}
