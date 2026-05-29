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

// clap's dynamic zsh script is built to be sourced; the Nix package installs it as an
// fpath autoload `_gradient` file, where the completer is otherwise registered only on
// the first TAB (producing nothing). The appended bridge must run it on first invocation.
#[test]
fn completion_zsh_bridges_autoload_first_tab() {
    let output = Command::cargo_bin("gradient")
        .unwrap()
        .args(["completion", "zsh"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let script = String::from_utf8(output.stdout).unwrap();
    assert!(
        script.contains(r#"[[ ${funcstack[1]} = _gradient ]] && _clap_dynamic_completer_gradient "$@""#),
        "zsh script must bridge the autoload first-TAB case:\n{script}"
    );
}
