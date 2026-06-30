/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![cfg(feature = "eval")]

use assert_cmd::Command;

#[test]
fn eval_help_describes_nix_eval_jobs_like_output() {
    let out = Command::cargo_bin("gradient")
        .unwrap()
        .args(["eval", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success(), "eval --help should succeed");
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        text.contains("nix-eval-jobs"),
        "help should reference nix-eval-jobs:\n{text}"
    );
    assert!(text.contains("--flake"), "missing --flake flag:\n{text}");
}

#[test]
fn eval_requires_a_pattern() {
    let out = Command::cargo_bin("gradient")
        .unwrap()
        .arg("eval")
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "eval with no pattern should fail with a usage error"
    );
}
