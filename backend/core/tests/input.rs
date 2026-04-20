/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Tests for `core::input` — all pure functions, no DB or I/O needed.

extern crate core as gradient_core;
use gradient_core::types::input::*;

// ── url_to_addr ───────────────────────────────────────────────────────────────

#[test]
fn url_to_addr_ipv4() {
    assert_eq!(
        url_to_addr("127.0.0.1", 8080).unwrap().to_string(),
        "127.0.0.1:8080"
    );
}

#[test]
fn url_to_addr_ipv6() {
    assert_eq!(url_to_addr("::1", 8080).unwrap().to_string(), "[::1]:8080");
}

#[test]
fn url_to_addr_localhost_resolves_to_loopback() {
    assert_eq!(
        url_to_addr("localhost", 8080).unwrap().to_string(),
        "[::1]:8080"
    );
}

#[test]
fn url_to_addr_port_zero_is_rejected() {
    assert_eq!(
        url_to_addr("127.0.0.1", 0).unwrap_err().to_string(),
        "Port 0 is out of range 1-65535"
    );
}

#[test]
fn url_to_addr_port_above_max_is_rejected() {
    assert_eq!(
        url_to_addr("127.0.0.1", 65536).unwrap_err().to_string(),
        "Port 65536 is out of range 1-65535"
    );
}

#[test]
fn url_to_addr_negative_port_is_rejected() {
    assert_eq!(
        url_to_addr("127.0.0.1", -1).unwrap_err().to_string(),
        "Port -1 is out of range 1-65535"
    );
}

// ── port_in_range ─────────────────────────────────────────────────────────────

#[test]
fn port_in_range_valid() {
    assert_eq!(port_in_range("8080").unwrap(), 8080);
    assert_eq!(port_in_range("65535").unwrap(), 65535);
    assert_eq!(port_in_range("1").unwrap(), 1);
}

#[test]
fn port_in_range_zero_rejected() {
    assert_eq!(
        port_in_range("0").unwrap_err().to_string(),
        "Port not in range 1-65535"
    );
}

#[test]
fn port_in_range_too_large_rejected() {
    assert_eq!(
        port_in_range("65536").unwrap_err().to_string(),
        "Port not in range 1-65535"
    );
}

// ── greater_than_zero ─────────────────────────────────────────────────────────

#[test]
fn greater_than_zero_valid() {
    assert_eq!(greater_than_zero::<u32>("1").unwrap(), 1);
    assert_eq!(greater_than_zero::<f32>("1.0").unwrap(), 1.0);
}

#[test]
fn greater_than_zero_zero_rejected() {
    assert_eq!(
        greater_than_zero::<usize>("0").unwrap_err().to_string(),
        "`0` is not larger than 0"
    );
}

#[test]
fn greater_than_zero_negative_rejected() {
    assert_eq!(
        greater_than_zero::<i32>("-1").unwrap_err().to_string(),
        "`-1` is not larger than 0"
    );
}

#[test]
fn greater_than_zero_non_numeric_rejected() {
    assert_eq!(
        greater_than_zero::<u32>("a").unwrap_err().to_string(),
        "`a` is not a valid number"
    );
}

// ── hex_to_vec / vec_to_hex ───────────────────────────────────────────────────

#[test]
fn hex_roundtrip() {
    let original = "a1b2c3d4e5f6789012345678901234567890abcd";
    assert_eq!(vec_to_hex(&hex_to_vec(original).unwrap()), original);
}

#[test]
fn hex_to_vec_decodes_correctly() {
    assert_eq!(hex_to_vec("68656c6c6f").unwrap(), b"hello");
}

#[test]
fn hex_to_vec_odd_length_rejected() {
    assert_eq!(
        hex_to_vec("68656c6c6").unwrap_err().to_string(),
        "Invalid hex string"
    );
}

#[test]
fn hex_to_vec_non_hex_char_rejected() {
    assert_eq!(
        hex_to_vec("68656c6c6g").unwrap_err().to_string(),
        "Invalid hex string"
    );
}

// ── repository_url_to_nix ────────────────────────────────────────────────────

const REV: &str = "11c2f8505c234697ccabbc96e5b8a76daf0f31d3";

#[test]
fn repository_url_ssh_scp_style() {
    let url = repository_url_to_nix("git@github.com:Wavelens/Gradient.git", REV).unwrap();
    assert_eq!(
        url,
        format!("git@github.com:Wavelens/Gradient.git?rev={REV}")
    );
}

#[test]
fn repository_url_https_gets_git_plus_prefix() {
    let url = repository_url_to_nix("https://github.com/Wavelens/Gradient.git", REV).unwrap();
    assert_eq!(
        url,
        format!("git+https://github.com/Wavelens/Gradient.git?rev={REV}")
    );
}

#[test]
fn repository_url_git_protocol_passthrough() {
    let url = repository_url_to_nix("git://server.example.com/repo.git", REV).unwrap();
    assert_eq!(url, format!("git://server.example.com/repo.git?rev={REV}"));
}

#[test]
fn repository_url_short_hash_rejected() {
    assert_eq!(
        repository_url_to_nix("https://github.com/foo/bar.git", "abc123")
            .unwrap_err()
            .to_string(),
        "Commit hash must be 40 characters long"
    );
}

#[test]
fn repository_url_file_scheme_rejected() {
    assert_eq!(
        repository_url_to_nix("file:///local/repo", REV)
            .unwrap_err()
            .to_string(),
        "URLs pointing to local files are not allowed"
    );
}

#[test]
fn repository_url_bare_file_prefix_rejected() {
    // `file:/local/repo` doesn't contain `file://` but still starts with `file`.
    assert_eq!(
        repository_url_to_nix("file:/local/repo", REV)
            .unwrap_err()
            .to_string(),
        "URLs pointing to local files are not allowed"
    );
}

// ── check_repository_url_is_ssh ──────────────────────────────────────────────

#[test]
fn ssh_url_detection() {
    assert!(check_repository_url_is_ssh(
        "git+ssh://git@github.com/user/repo.git"
    ));
    assert!(check_repository_url_is_ssh(
        "ssh://git@github.com/user/repo.git"
    ));
    assert!(check_repository_url_is_ssh("git@github.com:user/repo.git"));
    assert!(check_repository_url_is_ssh(
        "user@example.com:path/to/repo.git"
    ));
}

#[test]
fn https_is_not_ssh() {
    assert!(!check_repository_url_is_ssh(
        "https://github.com/user/repo.git"
    ));
    assert!(!check_repository_url_is_ssh(
        "http://github.com/user/repo.git"
    ));
    assert!(!check_repository_url_is_ssh(
        "https://user@github.com/repo.git"
    ));
    assert!(!check_repository_url_is_ssh("/local/path/to/repo.git"));
}

#[test]
fn https_with_userinfo_and_port_is_not_ssh() {
    // `user@host:port` inside an `https://` URL must not be mistaken for SCP.
    assert!(!check_repository_url_is_ssh(
        "https://user@github.com:8080/repo.git"
    ));
    assert!(!check_repository_url_is_ssh(
        "http://alice@example.com:443/path"
    ));
}

// ── check_index_name ─────────────────────────────────────────────────────────

#[test]
fn index_name_valid() {
    check_index_name("test").unwrap();
    check_index_name("te-st").unwrap();
    check_index_name("test1").unwrap();
    check_index_name("te-9st").unwrap();
}

#[test]
fn index_name_empty_rejected() {
    assert_eq!(
        check_index_name("").unwrap_err().to_string(),
        "Name cannot be empty"
    );
}

#[test]
fn index_name_uppercase_rejected() {
    assert_eq!(
        check_index_name("Test").unwrap_err().to_string(),
        "Name must be lowercase"
    );
}

#[test]
fn index_name_trailing_dash_rejected() {
    assert_eq!(
        check_index_name("test-").unwrap_err().to_string(),
        "Name can only start and end with letters or numbers"
    );
}

#[test]
fn index_name_underscore_rejected() {
    assert_eq!(
        check_index_name("test_").unwrap_err().to_string(),
        "Name can only contain letters, numbers, and dashes"
    );
}

#[test]
fn index_name_space_rejected() {
    assert_eq!(
        check_index_name("test name").unwrap_err().to_string(),
        "Name can only contain letters, numbers, and dashes"
    );
}

// ── validate_password ────────────────────────────────────────────────────────

#[test]
fn password_valid() {
    assert!(validate_password("StrongPass123!").is_ok());
    assert!(validate_password("MySecure@2024").is_ok());
    assert!(validate_password("Abc123!@").is_ok()); // exactly 8 chars
}

#[test]
fn password_too_short_rejected() {
    assert_eq!(
        validate_password("Ab1!").unwrap_err().to_string(),
        "Password must be at least 8 characters long"
    );
}

#[test]
fn password_too_long_rejected() {
    let long = "Ab1!".repeat(33); // 132 chars
    assert_eq!(
        validate_password(&long).unwrap_err().to_string(),
        "Password cannot exceed 128 characters"
    );
}

#[test]
fn password_exactly_128_chars_is_valid() {
    assert!(validate_password(&"Ab1!".repeat(32)).is_ok());
}

#[test]
fn password_missing_uppercase_rejected() {
    assert_eq!(
        validate_password("lowercase123!").unwrap_err().to_string(),
        "Password must contain at least one uppercase letter"
    );
}

#[test]
fn password_missing_lowercase_rejected() {
    assert_eq!(
        validate_password("UPPERCASE123!").unwrap_err().to_string(),
        "Password must contain at least one lowercase letter"
    );
}

#[test]
fn password_missing_digit_rejected() {
    assert_eq!(
        validate_password("NoDigitsHere!").unwrap_err().to_string(),
        "Password must contain at least one digit"
    );
}

#[test]
fn password_missing_special_char_rejected() {
    assert_eq!(
        validate_password("NoSpecial123").unwrap_err().to_string(),
        "Password must contain at least one special character (!@#$%^&*()_+-=[]{}|;:,.<>?)"
    );
}

#[test]
fn password_containing_word_password_rejected() {
    assert_eq!(
        validate_password("MyPassword123!").unwrap_err().to_string(),
        "Password cannot contain the word 'password'"
    );
}

#[test]
fn password_sequential_chars_rejected() {
    assert_eq!(
        validate_password("Testabcde123!").unwrap_err().to_string(),
        "Password cannot contain sequential characters (e.g., 'abcd', '1234')"
    );
    assert_eq!(
        validate_password("Test12345!").unwrap_err().to_string(),
        "Password cannot contain sequential characters (e.g., 'abcd', '1234')"
    );
}

#[test]
fn password_repeated_chars_rejected() {
    assert_eq!(
        validate_password("Testaaa123!").unwrap_err().to_string(),
        "Password cannot contain repeated characters (e.g., 'aaa', '111')"
    );
}

#[test]
fn password_non_sequential_alternating_is_valid() {
    assert!(validate_password("TestAaAa1!").is_ok());
    assert!(validate_password("Test1a1a!").is_ok());
}

// ── validate_username ────────────────────────────────────────────────────────

#[test]
fn username_valid() {
    assert!(validate_username("alice").is_ok());
    assert!(validate_username("alice_bob").is_ok());
    assert!(validate_username("alice-bob").is_ok());
    assert!(validate_username("a1b").is_ok()); // exactly 3 chars
    assert!(validate_username("Bob42").is_ok());
}

#[test]
fn username_empty_rejected() {
    assert_eq!(
        validate_username("").unwrap_err(),
        core::types::input::InputError::UsernameEmpty
    );
}

#[test]
fn username_too_short_rejected() {
    assert_eq!(
        validate_username("ab").unwrap_err().to_string(),
        "Username must be at least 3 characters long"
    );
}

#[test]
fn username_too_long_rejected() {
    let long = "a".repeat(51);
    assert_eq!(
        validate_username(&long).unwrap_err().to_string(),
        "Username cannot exceed 50 characters"
    );
}

#[test]
fn username_exactly_50_chars_is_valid() {
    assert!(validate_username(&"a".repeat(50)).is_ok());
}

#[test]
fn username_invalid_chars_rejected() {
    assert_eq!(
        validate_username("alice!bob").unwrap_err().to_string(),
        "Username can only contain letters, numbers, underscores, and hyphens"
    );
    assert_eq!(
        validate_username("alice.bob").unwrap_err().to_string(),
        "Username can only contain letters, numbers, underscores, and hyphens"
    );
    assert_eq!(
        validate_username("alice bob").unwrap_err().to_string(),
        "Username can only contain letters, numbers, underscores, and hyphens"
    );
}

#[test]
fn username_bad_start_end_rejected() {
    let msg = "Username cannot start or end with underscore or hyphen";
    assert_eq!(validate_username("_alice").unwrap_err().to_string(), msg);
    assert_eq!(validate_username("-alice").unwrap_err().to_string(), msg);
    assert_eq!(validate_username("alice_").unwrap_err().to_string(), msg);
    assert_eq!(validate_username("alice-").unwrap_err().to_string(), msg);
}

#[test]
fn username_consecutive_specials_rejected() {
    let msg = "Username cannot contain consecutive special characters";
    assert_eq!(validate_username("a__b").unwrap_err().to_string(), msg);
    assert_eq!(validate_username("a--b").unwrap_err().to_string(), msg);
    assert_eq!(validate_username("a_-b").unwrap_err().to_string(), msg);
    assert_eq!(validate_username("a-_b").unwrap_err().to_string(), msg);
}

#[test]
fn username_reserved_rejected() {
    let msg = "This username is reserved and cannot be used";
    assert_eq!(validate_username("admin").unwrap_err().to_string(), msg);
    assert_eq!(validate_username("ROOT").unwrap_err().to_string(), msg);
    assert_eq!(validate_username("System").unwrap_err().to_string(), msg);
    assert_eq!(validate_username("undefined").unwrap_err().to_string(), msg);
}

// ── validate_display_name ────────────────────────────────────────────────────

#[test]
fn display_name_valid() {
    assert!(validate_display_name("Alice").is_ok());
    assert!(validate_display_name("Alice Bob").is_ok());
    assert!(validate_display_name("O'Neil").is_ok());
    assert!(validate_display_name("J.R.R. Tolkien").is_ok());
    assert!(validate_display_name("name-with_under").is_ok());
}

#[test]
fn display_name_empty_rejected() {
    assert_eq!(
        validate_display_name("").unwrap_err().to_string(),
        "Display name cannot be empty"
    );
}

#[test]
fn display_name_too_long_rejected() {
    assert_eq!(
        validate_display_name(&"a".repeat(101))
            .unwrap_err()
            .to_string(),
        "Display name cannot exceed 100 characters"
    );
}

#[test]
fn display_name_exactly_100_is_valid() {
    assert!(validate_display_name(&"a".repeat(100)).is_ok());
}

#[test]
fn display_name_invalid_chars_rejected() {
    assert_eq!(
        validate_display_name("Alice!").unwrap_err().to_string(),
        "Display name can only contain letters, numbers, spaces, apostrophes, dots, dashes, and underscores"
    );
}

#[test]
fn display_name_leading_or_trailing_space_rejected() {
    let msg = "Display name cannot start or end with spaces";
    assert_eq!(validate_display_name(" Alice").unwrap_err().to_string(), msg);
    assert_eq!(validate_display_name("Alice ").unwrap_err().to_string(), msg);
}

#[test]
fn display_name_consecutive_spaces_rejected() {
    assert_eq!(
        validate_display_name("Alice  Bob").unwrap_err().to_string(),
        "Display name cannot contain consecutive spaces"
    );
}
