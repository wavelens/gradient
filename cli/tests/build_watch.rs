/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use assert_cmd::Command;

fn help(args: &[&str]) -> String {
    let output = Command::cargo_bin("gradient")
        .unwrap()
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "command failed: {args:?}");
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn build_exposes_background_flag_not_no_stream() {
    let text = help(&["build", "--help"]);
    assert!(text.contains("--background"), "missing --background:\n{text}");
    assert!(text.contains("-b"), "missing -b short flag:\n{text}");
    assert!(
        !text.contains("--no-stream"),
        "--no-stream should be replaced by --background:\n{text}"
    );
}

#[test]
fn watch_command_takes_an_evaluation() {
    let text = help(&["watch", "--help"]);
    assert!(text.contains("evaluation"), "missing evaluation arg:\n{text}");
}

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
