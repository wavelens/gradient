/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

fn main() {
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");

    let output = std::process::Command::new("pkg-config")
        .args(["--libs", "nix-flake-c"])
        .output()
        .expect("pkg-config not found; nix development headers are required to build this crate");

    if !output.status.success() {
        panic!(
            "pkg-config --libs nix-flake-c failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let flags = String::from_utf8(output.stdout).expect("pkg-config output is not valid UTF-8");
    for flag in flags.split_whitespace() {
        if let Some(path) = flag.strip_prefix("-L") {
            println!("cargo:rustc-link-search=native={path}");
        } else if let Some(lib) = flag.strip_prefix("-l") {
            println!("cargo:rustc-link-lib={lib}");
        }
    }
}
