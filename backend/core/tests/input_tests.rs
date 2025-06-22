/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Tests for input validation and parsing functions

extern crate core as gradient_core;
use git_url_parse::normalize_url;
use gradient_core::input::*;

#[test]
fn test_url_to_addr() {
    let addr = url_to_addr("127.0.0.1", 8080).unwrap();
    assert_eq!(addr.to_string(), "127.0.0.1:8080");

    let addr = url_to_addr("localhost", 8080).unwrap();
    assert_eq!(addr.to_string(), "[::1]:8080");

    let addr = url_to_addr("127.0.0.1", 65536).unwrap_err();
    assert_eq!(addr.to_string(), "port out of range 1-65535");

    let addr = url_to_addr("127.0.0.1", 0).unwrap_err();
    assert_eq!(addr.to_string(), "port out of range 1-65535");

    let addr = url_to_addr("127.0.0.1", -1).unwrap_err();
    assert_eq!(addr.to_string(), "port out of range 1-65535");

    let addr = url_to_addr("::1", 8080).unwrap();
    assert_eq!(addr.to_string(), "[::1]:8080");

    let addr = url_to_addr(":::1", 8080).unwrap_err();
    assert_eq!(
        addr.to_string(),
        "failed to lookup address information: Name or service not known"
    );
}

#[test]
fn test_port_in_range() {
    let port = port_in_range("8080").unwrap();
    assert_eq!(port, 8080);

    let port = port_in_range("65535").unwrap();
    assert_eq!(port, 65535);

    let port = port_in_range("65536").unwrap_err();
    assert_eq!(port, "port not in range 1-65535");

    let port = port_in_range("0").unwrap_err();
    assert_eq!(port, "port not in range 1-65535");
}

#[test]
fn test_greater_than_zero() {
    let num = greater_than_zero::<u32>("1").unwrap();
    assert_eq!(num, 1);

    let num = greater_than_zero::<usize>("0").unwrap_err();
    assert_eq!(num, "`0` is not larger than 0");

    let num = greater_than_zero::<u32>("-1").unwrap_err();
    assert_eq!(num, "`-1` is not a valid number");

    let num = greater_than_zero::<i32>("-1").unwrap_err();
    assert_eq!(num, "`-1` is not larger than 0");

    let num = greater_than_zero::<u32>("a").unwrap_err();
    assert_eq!(num, "`a` is not a valid number");

    let num = greater_than_zero::<f32>("1.0").unwrap();
    assert_eq!(num, 1.0);
}

#[test]
fn test_hex_to_vec() {
    let vec = hex_to_vec("68656c6c6f").unwrap();
    assert_eq!(vec, vec![0x68, 0x65, 0x6c, 0x6c, 0x6f]);

    let vec = hex_to_vec("11c2f8505c234697ccabbc96e5b8a76daf0f31d3").unwrap();
    assert_eq!(
        vec,
        vec![
            0x11, 0xc2, 0xf8, 0x50, 0x5c, 0x23, 0x46, 0x97, 0xcc, 0xab, 0xbc, 0x96, 0xe5, 0xb8,
            0xa7, 0x6d, 0xaf, 0x0f, 0x31, 0xd3
        ]
    );

    let vec = hex_to_vec("68656c6c6").unwrap_err();
    assert_eq!(vec.to_string(), "invalid hex string");

    let vec = hex_to_vec("68656c6c6g").unwrap_err();
    assert_eq!(vec.to_string(), "invalid digit found in string");
}

#[test]
fn test_vec_to_hex() {
    let test_vec = vec![0xa1, 0xb2, 0xc3, 0xd4, 0xe5, 0xf6, 0x78, 0x90];
    let result = vec_to_hex(&test_vec);
    assert_eq!(result, "a1b2c3d4e5f67890");
}

#[test]
fn test_hex_to_vec_conversion() {
    let test_hash = "a1b2c3d4e5f6789012345678901234567890abcd";
    let result = hex_to_vec(test_hash).unwrap();
    let converted_back = vec_to_hex(&result);
    assert_eq!(test_hash, converted_back);
}

#[test]
fn test_repository_url_to_nix() {
    let url = repository_url_to_nix(
        normalize_url("git@github.com:Wavelens/Gradient.git")
            .unwrap()
            .as_str(),
        "11c2f8505c234697ccabbc96e5b8a76daf0f31d3",
    )
    .unwrap();
    assert_eq!(
        url,
        "git+ssh://git@github.com/Wavelens/Gradient.git?rev=11c2f8505c234697ccabbc96e5b8a76daf0f31d3"
    );

    let url = repository_url_to_nix(
        normalize_url("https://github.com/Wavelens/Gradient.git")
            .unwrap()
            .as_str(),
        "11c2f8505c234697ccabbc96e5b8a76daf0f31d3",
    )
    .unwrap();
    assert_eq!(
        url,
        "git+https://github.com/Wavelens/Gradient.git?rev=11c2f8505c234697ccabbc96e5b8a76daf0f31d3"
    );
}

#[test]
fn test_check_repository_url_is_ssh() {
    use gradient_core::input::check_repository_url_is_ssh;

    assert!(check_repository_url_is_ssh(
        "git+ssh://git@github.com/user/repo.git"
    ));
    assert!(!check_repository_url_is_ssh(
        "https://github.com/user/repo.git"
    ));
    assert!(!check_repository_url_is_ssh(
        "http://github.com/user/repo.git"
    ));
}

#[test]
fn test_check_index_name() {
    check_index_name("test").unwrap();
    check_index_name("te-st").unwrap();
    check_index_name("test1").unwrap();
    check_index_name("te-9st").unwrap();

    let name = check_index_name("Test").unwrap_err();
    assert_eq!(name, "Name must be lowercase");

    let name = check_index_name("test-").unwrap_err();
    assert_eq!(name, "Name can only start and end with letters or numbers");

    let name = check_index_name("test_").unwrap_err();
    assert_eq!(name, "Name can only contain letters, numbers, and dashes");

    let name = check_index_name("test ").unwrap_err();
    assert_eq!(name, "Name can only contain letters, numbers, and dashes");

    let name = check_index_name("test name").unwrap_err();
    assert_eq!(name, "Name can only contain letters, numbers, and dashes");

    let name = check_index_name("test?name").unwrap_err();
    assert_eq!(name, "Name can only contain letters, numbers, and dashes");

    let name = check_index_name("").unwrap_err();
    assert_eq!(name, "Name cannot be empty");
}

#[test]
fn test_parse_evaluation_wildcard() {
    use gradient_core::input::parse_evaluation_wildcard;

    // Valid wildcards
    let result = parse_evaluation_wildcard("*.nix").unwrap();
    assert_eq!(result, vec!["*.nix"]);

    let result = parse_evaluation_wildcard("*.nix,*.toml").unwrap();
    assert_eq!(result, vec!["*.nix", "*.toml"]);

    // Invalid wildcards
    assert!(parse_evaluation_wildcard("").is_err());
    assert!(parse_evaluation_wildcard("test,,test").is_err());
    assert!(parse_evaluation_wildcard(" *.nix").is_err());
    assert!(parse_evaluation_wildcard("*.nix ").is_err());
}

#[test]
fn test_valid_evaluation_wildcard() {
    use gradient_core::input::valid_evaluation_wildcard;

    assert!(valid_evaluation_wildcard("*.nix"));
    assert!(valid_evaluation_wildcard("*.nix,*.toml"));
    assert!(!valid_evaluation_wildcard(""));
    assert!(!valid_evaluation_wildcard("test,,test"));
}
