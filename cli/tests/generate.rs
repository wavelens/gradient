/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use assert_cmd::Command;
use sha2::{Digest, Sha256};

#[test]
fn generate_apikey_json_prints_the_token_with_its_matching_digest() {
    let output = Command::cargo_bin("gradient")
        .unwrap()
        .args(["--json", "generate", "apikey"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let env: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(env["error"], false);

    let token = env["message"]["token"].as_str().unwrap();
    let digest = env["message"]["key_file_digest"].as_str().unwrap();
    let expected: String = Sha256::digest(token.strip_prefix("GRAD").unwrap().as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(digest, expected);
}
