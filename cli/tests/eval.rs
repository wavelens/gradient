/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![cfg(feature = "eval")]

use assert_cmd::Command;

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
