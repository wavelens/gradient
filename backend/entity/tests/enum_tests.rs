/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Tests for entity enums

use entity::*;
use std::str::FromStr;

#[test]
fn test_architecture_from_str() {
    assert_eq!(
        server::Architecture::from_str("x86_64-linux").unwrap(),
        server::Architecture::X86_64Linux
    );
    assert_eq!(
        server::Architecture::from_str("aarch64-linux").unwrap(),
        server::Architecture::Aarch64Linux
    );
    assert_eq!(
        server::Architecture::from_str("x86_64-darwin").unwrap(),
        server::Architecture::X86_64Darwin
    );
    assert_eq!(
        server::Architecture::from_str("aarch64-darwin").unwrap(),
        server::Architecture::Aarch64Darwin
    );

    assert!(server::Architecture::from_str("invalid-arch").is_err());
}
