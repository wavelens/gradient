/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_web::ip_allowlist::{is_allowed, normalize_entry};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(a, b, c, d))
}

#[test]
fn is_allowed_filters_by_entries_and_skips_malformed_ones() {
    assert!(is_allowed(v4(1, 2, 3, 4), &[]));

    let exact = vec!["203.0.113.5/32".to_string()];
    assert!(is_allowed(v4(203, 0, 113, 5), &exact));
    assert!(!is_allowed(v4(203, 0, 113, 6), &exact));

    let net = vec!["10.0.0.0/24".to_string()];
    assert!(is_allowed(v4(10, 0, 0, 1), &net));
    assert!(is_allowed(v4(10, 0, 0, 254), &net));
    assert!(!is_allowed(v4(10, 0, 1, 0), &net));

    let mapped = IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0x0a00, 0x0001));
    assert!(is_allowed(mapped, &net));

    let with_garbage = vec!["not-an-ip".to_string(), "10.0.0.0/24".to_string()];
    assert!(is_allowed(v4(10, 0, 0, 1), &with_garbage));

    let other = vec!["172.16.0.0/12".to_string()];
    assert!(!is_allowed(v4(192, 168, 1, 1), &other));
}

#[test]
fn normalize_entry_canonicalizes_and_rejects_garbage() {
    for (input, expected) in [
        ("203.0.113.5", "203.0.113.5/32"),
        ("2001:db8::1", "2001:db8::1/128"),
        ("10.0.0.0/8", "10.0.0.0/8"),
        ("  10.0.0.0/8  ", "10.0.0.0/8"),
    ] {
        assert_eq!(normalize_entry(input).unwrap(), expected, "{input}");
    }
    for input in ["hello world", "   "] {
        assert!(normalize_entry(input).is_err(), "{input:?}");
    }
}
