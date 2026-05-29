/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use assert_cmd::Command;

// Regression for the broken completion bin name: the generated script must
// register against the real `gradient` binary, never the `Gradient` app name.
#[test]
fn completion_bash_registers_lowercase_binary() {
    let output = Command::cargo_bin("gradient")
        .unwrap()
        .args(["completion", "bash"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let script = String::from_utf8(output.stdout).unwrap();
    assert!(
        script.contains("_clap_complete_gradient"),
        "completion function should be _clap_complete_gradient:\n{script}"
    );
    assert!(
        script.contains("-F _clap_complete_gradient gradient"),
        "completion must be registered against the gradient binary:\n{script}"
    );
    assert!(
        !script.contains("Gradient"),
        "completion script must not reference the capitalised app name:\n{script}"
    );
}

#[test]
fn completion_zsh_registers_lowercase_binary() {
    let output = Command::cargo_bin("gradient")
        .unwrap()
        .args(["completion", "zsh"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let script = String::from_utf8(output.stdout).unwrap();
    assert!(!script.contains("Gradient"), "zsh script: {script}");
    assert!(script.contains("gradient"), "zsh script: {script}");
}
