/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_core::ip_allowlist::{is_allowed, normalize_entry};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(a, b, c, d))
}

#[test]
fn empty_list_allows_everything() {
    assert!(is_allowed(v4(1, 2, 3, 4), &[]));
}

#[test]
fn slash_32_exact_match() {
    let list = vec!["203.0.113.5/32".to_string()];
    assert!(is_allowed(v4(203, 0, 113, 5), &list));
    assert!(!is_allowed(v4(203, 0, 113, 6), &list));
}

#[test]
fn slash_24_contains_address() {
    let list = vec!["10.0.0.0/24".to_string()];
    assert!(is_allowed(v4(10, 0, 0, 1), &list));
    assert!(is_allowed(v4(10, 0, 0, 254), &list));
    assert!(!is_allowed(v4(10, 0, 1, 0), &list));
}

#[test]
fn ipv4_mapped_ipv6_matches_ipv4_cidr() {
    let list = vec!["10.0.0.0/24".to_string()];
    let mapped = IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0x0a00, 0x0001));
    assert!(is_allowed(mapped, &list));
}

#[test]
fn malformed_entry_is_skipped_but_others_still_count() {
    let list = vec!["not-an-ip".to_string(), "10.0.0.0/24".to_string()];
    assert!(is_allowed(v4(10, 0, 0, 1), &list));
}

#[test]
fn no_match_when_all_entries_fail() {
    let list = vec!["172.16.0.0/12".to_string()];
    assert!(!is_allowed(v4(192, 168, 1, 1), &list));
}

#[test]
fn normalize_bare_ipv4_to_slash_32() {
    assert_eq!(normalize_entry("203.0.113.5").unwrap(), "203.0.113.5/32");
}

#[test]
fn normalize_bare_ipv6_to_slash_128() {
    assert_eq!(normalize_entry("2001:db8::1").unwrap(), "2001:db8::1/128");
}

#[test]
fn normalize_keeps_cidr_unchanged() {
    assert_eq!(normalize_entry("10.0.0.0/8").unwrap(), "10.0.0.0/8");
}

#[test]
fn normalize_trims_whitespace() {
    assert_eq!(normalize_entry("  10.0.0.0/8  ").unwrap(), "10.0.0.0/8");
}

#[test]
fn normalize_rejects_garbage() {
    assert!(normalize_entry("hello world").is_err());
}

#[test]
fn normalize_rejects_empty() {
    assert!(normalize_entry("   ").is_err());
}
