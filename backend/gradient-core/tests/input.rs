/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::input::*;
use gradient_types::input::{InputError, check_task_name};

const REV: &str = "11c2f8505c234697ccabbc96e5b8a76daf0f31d3";

#[test]
fn url_to_addr_resolves_host_to_socket_addr() {
    for (host, expected) in [("127.0.0.1", "127.0.0.1:8080"), ("::1", "[::1]:8080")] {
        assert_eq!(
            url_to_addr(host, 8080).unwrap().to_string(),
            expected,
            "{host}"
        );
    }
    let localhost = url_to_addr("localhost", 8080).unwrap();
    assert!(
        localhost.ip().is_loopback(),
        "expected loopback, got {localhost}"
    );
    assert_eq!(localhost.port(), 8080);
}

#[test]
fn url_to_addr_rejects_a_port_out_of_range() {
    let cases = [
        (0, "Port 0 is out of range 1-65535"),
        (65536, "Port 65536 is out of range 1-65535"),
        (-1, "Port -1 is out of range 1-65535"),
    ];
    for (port, expected) in cases {
        assert_eq!(
            url_to_addr("127.0.0.1", port).unwrap_err().to_string(),
            expected,
            "{port}"
        );
    }
}

#[test]
fn port_in_range_accepts_the_bounds_and_rejects_outside() {
    for (input, expected) in [("8080", 8080), ("65535", 65535), ("1", 1)] {
        assert_eq!(port_in_range(input).unwrap(), expected, "{input}");
    }
    for input in ["0", "65536"] {
        assert_eq!(
            port_in_range(input).unwrap_err().to_string(),
            "Port not in range 1-65535",
            "{input}"
        );
    }
}

#[test]
fn hex_codec_round_trips_and_rejects_malformed_input() {
    let original = "a1b2c3d4e5f6789012345678901234567890abcd";
    assert_eq!(vec_to_hex(&hex_to_vec(original).unwrap()), original);
    assert_eq!(hex_to_vec("68656c6c6f").unwrap(), b"hello");
    for bad in ["68656c6c6", "68656c6c6g"] {
        assert_eq!(
            hex_to_vec(bad).unwrap_err().to_string(),
            "Invalid hex string",
            "{bad}"
        );
    }
}

#[test]
fn repository_url_to_nix_normalizes_the_transport() {
    let cases = [
        (
            "git@github.com:Wavelens/Gradient.git",
            format!("git@github.com:Wavelens/Gradient.git?rev={REV}"),
        ),
        (
            "https://github.com/Wavelens/Gradient.git",
            format!("git+https://github.com/Wavelens/Gradient.git?rev={REV}"),
        ),
        (
            "git://server.example.com/repo.git",
            format!("git://server.example.com/repo.git?rev={REV}"),
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(
            repository_url_to_nix(input, REV).unwrap(),
            expected,
            "{input}"
        );
    }
}

#[test]
fn repository_url_to_nix_rejects_local_files_and_short_revs() {
    let local = "URLs pointing to local files are not allowed";
    let cases = [
        (
            "https://github.com/foo/bar.git",
            "abc123",
            "Commit hash must be 40 characters long",
        ),
        ("file:///local/repo", REV, local),
        ("file:/local/repo", REV, local),
    ];
    for (input, rev, expected) in cases {
        assert_eq!(
            repository_url_to_nix(input, rev).unwrap_err().to_string(),
            expected,
            "{input}"
        );
    }
}

#[test]
fn ssh_detection_matches_only_scp_and_ssh_transports() {
    let ssh = [
        "git+ssh://git@github.com/user/repo.git",
        "ssh://git@github.com/user/repo.git",
        "git@github.com:user/repo.git",
        "user@example.com:path/to/repo.git",
    ];
    for url in ssh {
        assert!(check_repository_url_is_ssh(url), "{url}");
    }
    let other = [
        "https://github.com/user/repo.git",
        "http://github.com/user/repo.git",
        "https://user@github.com/repo.git",
        "/local/path/to/repo.git",
        "https://user@github.com:8080/repo.git",
        "http://alice@example.com:443/path",
    ];
    for url in other {
        assert!(!check_repository_url_is_ssh(url), "{url}");
    }
}

#[test]
fn index_name_accepts_lowercase_alphanumerics_with_dashes() {
    for name in ["test", "te-st", "test1", "te-9st"] {
        check_index_name(name).unwrap();
    }
}

fn message_of(actual: String, expected: &str, label: &str) {
    assert!(
        actual.contains(expected),
        "{label}: expected a message containing {expected:?}, got {actual:?}"
    );
}

#[test]
fn index_name_rejects_each_invalid_shape() {
    let cases = [
        ("", "cannot be empty"),
        ("Test", "must be lowercase"),
        ("test-", "start and end with letters or numbers"),
        ("test_", "can only contain letters, numbers"),
        ("test name", "can only contain letters, numbers"),
    ];
    for (name, expected) in cases {
        message_of(
            check_index_name(name).unwrap_err().to_string(),
            expected,
            name,
        );
    }
}

#[test]
fn task_name_rejects_the_reserved_name_but_allows_similar() {
    assert_eq!(
        check_task_name("build-request").unwrap_err().to_string(),
        "This task name is reserved and cannot be used"
    );
    for name in ["build-requests", "buildrequest", "my-build-request"] {
        check_task_name(name).unwrap();
    }
    assert_eq!(
        check_task_name("Build-Request"),
        Err(InputError::NameNotLowercase)
    );
    assert_eq!(check_task_name(""), Err(InputError::NameEmpty));
}

#[test]
fn password_accepts_valid_input_up_to_the_length_bound() {
    for pw in [
        "StrongPass123!",
        "MySecure@2024",
        "Abc123!@",
        "TestAaAa1!",
        "Test1a1a!",
    ] {
        assert!(validate_password(pw).is_ok(), "{pw}");
    }
    assert!(validate_password(&"Ab1!".repeat(32)).is_ok());
}

#[test]
fn password_rejects_each_rule_violation() {
    let cases: Vec<(String, &str)> = vec![
        ("Ab1!".into(), "8 characters"),
        ("Ab1!".repeat(33), "exceed 128 characters"),
        ("lowercase123!".into(), "uppercase letter"),
        ("UPPERCASE123!".into(), "lowercase letter"),
        ("NoDigitsHere!".into(), "digit"),
        ("NoSpecial123".into(), "special character"),
        ("MyPassword123!".into(), "word 'password'"),
        ("Testabcde123!".into(), "sequential characters"),
        ("Test12345!".into(), "sequential characters"),
        ("Testaaa123!".into(), "repeated characters"),
    ];
    for (pw, expected) in cases {
        message_of(
            validate_password(&pw).unwrap_err().to_string(),
            expected,
            &pw,
        );
    }
}

#[test]
fn username_accepts_valid_shapes() {
    for name in ["alice", "alice_bob", "alice-bob", "a1b", "Bob42"] {
        assert!(validate_username(name).is_ok(), "{name}");
    }
    assert!(validate_username(&"a".repeat(50)).is_ok());
}

#[test]
fn username_rejects_each_rule_violation() {
    let cases: Vec<(String, &str)> = vec![
        ("ab".into(), "3 characters"),
        ("a".repeat(51), "exceed 50 characters"),
        ("alice!bob".into(), "can only contain letters, numbers"),
        ("alice.bob".into(), "can only contain letters, numbers"),
        ("alice bob".into(), "can only contain letters, numbers"),
        ("_alice".into(), "cannot start or end with underscore"),
        ("-alice".into(), "cannot start or end with underscore"),
        ("alice_".into(), "cannot start or end with underscore"),
        ("alice-".into(), "cannot start or end with underscore"),
        ("a__b".into(), "consecutive special characters"),
        ("a--b".into(), "consecutive special characters"),
        ("a_-b".into(), "consecutive special characters"),
        ("a-_b".into(), "consecutive special characters"),
        ("admin".into(), "reserved and cannot be used"),
        ("ROOT".into(), "reserved and cannot be used"),
        ("System".into(), "reserved and cannot be used"),
        ("undefined".into(), "reserved and cannot be used"),
    ];
    for (name, expected) in cases {
        message_of(
            validate_username(&name).unwrap_err().to_string(),
            expected,
            &name,
        );
    }
    assert_eq!(
        validate_username("").unwrap_err(),
        InputError::UsernameEmpty
    );
}

#[test]
fn display_name_accepts_valid_shapes() {
    for name in [
        "Alice",
        "Alice Bob",
        "O'Neil",
        "J.R.R. Tolkien",
        "name-with_under",
    ] {
        assert!(validate_display_name(name).is_ok(), "{name}");
    }
    assert!(validate_display_name(&"a".repeat(100)).is_ok());
}

#[test]
fn display_name_rejects_each_rule_violation() {
    let cases: Vec<(String, &str)> = vec![
        (String::new(), "cannot be empty"),
        ("a".repeat(101), "exceed 100 characters"),
        ("Alice!".into(), "can only contain letters, numbers"),
        (" Alice".into(), "cannot start or end with spaces"),
        ("Alice ".into(), "cannot start or end with spaces"),
        ("Alice  Bob".into(), "consecutive spaces"),
    ];
    for (name, expected) in cases {
        message_of(
            validate_display_name(&name).unwrap_err().to_string(),
            expected,
            &name,
        );
    }
}
