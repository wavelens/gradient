/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use assert_cmd::Command;

#[test]
fn watch_requires_an_evaluation_argument() {
    let output = Command::cargo_bin("gradient")
        .unwrap()
        .arg("watch")
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "watch without an evaluation argument must fail"
    );
}
