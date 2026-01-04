/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Tests for input validation and parsing functions

extern crate core as gradient_core;
use gradient_core::input::*;

#[test]
fn test_url_to_addr() {
    let addr = url_to_addr("127.0.0.1", 8080).unwrap();
    assert_eq!(addr.to_string(), "127.0.0.1:8080");

    let addr = url_to_addr("localhost", 8080).unwrap();
    assert_eq!(addr.to_string(), "[::1]:8080");

    let addr = url_to_addr("127.0.0.1", 65536).unwrap_err();
    assert_eq!(addr.to_string(), "Port 65536 is out of range 1-65535");

    let addr = url_to_addr("127.0.0.1", 0).unwrap_err();
    assert_eq!(addr.to_string(), "Port 0 is out of range 1-65535");

    let addr = url_to_addr("127.0.0.1", -1).unwrap_err();
    assert_eq!(addr.to_string(), "Port -1 is out of range 1-65535");

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
    assert_eq!(port.to_string(), "Port not in range 1-65535");

    let port = port_in_range("0").unwrap_err();
    assert_eq!(port.to_string(), "Port not in range 1-65535");
}

#[test]
fn test_greater_than_zero() {
    let num = greater_than_zero::<u32>("1").unwrap();
    assert_eq!(num, 1);

    let num = greater_than_zero::<usize>("0").unwrap_err();
    assert_eq!(num.to_string(), "`0` is not larger than 0");

    let num = greater_than_zero::<u32>("-1").unwrap_err();
    assert_eq!(num.to_string(), "`-1` is not a valid number");

    let num = greater_than_zero::<i32>("-1").unwrap_err();
    assert_eq!(num.to_string(), "`-1` is not larger than 0");

    let num = greater_than_zero::<u32>("a").unwrap_err();
    assert_eq!(num.to_string(), "`a` is not a valid number");

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
    assert_eq!(vec.to_string(), "Invalid hex string");

    let vec = hex_to_vec("68656c6c6g").unwrap_err();
    assert_eq!(vec.to_string(), "Invalid hex string");
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
        "git@github.com:Wavelens/Gradient.git",
        "11c2f8505c234697ccabbc96e5b8a76daf0f31d3",
    )
    .unwrap();
    assert_eq!(
        url,
        "git@github.com:Wavelens/Gradient.git?rev=11c2f8505c234697ccabbc96e5b8a76daf0f31d3"
    );

    let url = repository_url_to_nix(
        "https://github.com/Wavelens/Gradient.git",
        "11c2f8505c234697ccabbc96e5b8a76daf0f31d3",
    )
    .unwrap();
    assert_eq!(
        url,
        "git+https://github.com/Wavelens/Gradient.git?rev=11c2f8505c234697ccabbc96e5b8a76daf0f31d3"
    );

    // Test git:// URL handling
    let url = repository_url_to_nix(
        "git://server.example.com/repo.git",
        "11c2f8505c234697ccabbc96e5b8a76daf0f31d3",
    )
    .unwrap();
    assert_eq!(
        url,
        "git://server.example.com/repo.git?rev=11c2f8505c234697ccabbc96e5b8a76daf0f31d3"
    );
}

#[test]
fn test_check_repository_url_is_ssh() {
    use gradient_core::input::check_repository_url_is_ssh;

    // git+ssh:// protocol
    assert!(check_repository_url_is_ssh(
        "git+ssh://git@github.com/user/repo.git"
    ));

    // ssh:// protocol
    assert!(check_repository_url_is_ssh(
        "ssh://git@github.com/user/repo.git"
    ));

    // SCP-like syntax (most common format)
    assert!(check_repository_url_is_ssh("git@github.com:user/repo.git"));
    assert!(check_repository_url_is_ssh("git@gitlab.com:user/repo.git"));
    assert!(check_repository_url_is_ssh(
        "user@example.com:path/to/repo.git"
    ));

    // HTTPS/HTTP should not be detected as SSH
    assert!(!check_repository_url_is_ssh(
        "https://github.com/user/repo.git"
    ));
    assert!(!check_repository_url_is_ssh(
        "http://github.com/user/repo.git"
    ));

    // Edge cases
    assert!(!check_repository_url_is_ssh("https://user@github.com/repo.git")); // HTTPS with user
    assert!(!check_repository_url_is_ssh("/local/path/to/repo.git")); // Local path
}

#[test]
fn test_check_index_name() {
    check_index_name("test").unwrap();
    check_index_name("te-st").unwrap();
    check_index_name("test1").unwrap();
    check_index_name("te-9st").unwrap();

    let name = check_index_name("Test").unwrap_err();
    assert_eq!(name.to_string(), "Name must be lowercase");

    let name = check_index_name("test-").unwrap_err();
    assert_eq!(name.to_string(), "Name can only start and end with letters or numbers");

    let name = check_index_name("test_").unwrap_err();
    assert_eq!(name.to_string(), "Name can only contain letters, numbers, and dashes");

    let name = check_index_name("test ").unwrap_err();
    assert_eq!(name.to_string(), "Name can only contain letters, numbers, and dashes");

    let name = check_index_name("test name").unwrap_err();
    assert_eq!(name.to_string(), "Name can only contain letters, numbers, and dashes");

    let name = check_index_name("test?name").unwrap_err();
    assert_eq!(name.to_string(), "Name can only contain letters, numbers, and dashes");

    let name = check_index_name("").unwrap_err();
    assert_eq!(name.to_string(), "Name cannot be empty");
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

#[test]
fn test_validate_password_valid() {
    // Valid passwords that meet all requirements
    assert!(validate_password("StrongPass123!").is_ok());
    assert!(validate_password("MySecure@2024").is_ok());
    assert!(validate_password("Complex#Pass9").is_ok());
    assert!(validate_password("Valid$123Abc").is_ok());
    assert!(validate_password("Testing@2025!").is_ok());
    assert!(validate_password("GoodP@ssw0rd").is_ok());

    // Valid with different special characters
    assert!(validate_password("Test123#").is_ok());
    assert!(validate_password("Test123$").is_ok());
    assert!(validate_password("Test123%").is_ok());
    assert!(validate_password("Test123^").is_ok());
    assert!(validate_password("Test123&").is_ok());
    assert!(validate_password("Test123*").is_ok());
    assert!(validate_password("Test123()").is_ok());
    assert!(validate_password("Test123_").is_ok());
    assert!(validate_password("Test123+").is_ok());
    assert!(validate_password("Test123-").is_ok());
    assert!(validate_password("Test123=").is_ok());
    assert!(validate_password("Test123[]").is_ok());
    assert!(validate_password("Test123{}").is_ok());
    assert!(validate_password("Test123|").is_ok());
    assert!(validate_password("Test123;").is_ok());
    assert!(validate_password("Test123:").is_ok());
    assert!(validate_password("Test123,").is_ok());
    assert!(validate_password("Test123.").is_ok());
    assert!(validate_password("Test123<").is_ok());
    assert!(validate_password("Test123>").is_ok());
    assert!(validate_password("Test123?").is_ok());
}

#[test]
fn test_validate_password_length_errors() {
    // Too short
    let result = validate_password("Abc1!");
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().to_string(),
        "Password must be at least 8 characters long"
    );

    let result = validate_password("Ab1!");
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().to_string(),
        "Password must be at least 8 characters long"
    );

    let result = validate_password("");
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().to_string(),
        "Password must be at least 8 characters long"
    );

    // Too long (over 128 characters)
    let long_password = "Ab1!".repeat(33); // 132 characters
    let result = validate_password(&long_password);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().to_string(), "Password cannot exceed 128 characters");

    // Exactly 128 characters should be valid
    let max_password = "Ab1!".repeat(32); // 128 characters
    assert!(validate_password(&max_password).is_ok());
}

#[test]
fn test_validate_password_complexity_errors() {
    // Missing uppercase
    let result = validate_password("lowercase123!");
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().to_string(),
        "Password must contain at least one uppercase letter"
    );

    // Missing lowercase
    let result = validate_password("UPPERCASE123!");
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().to_string(),
        "Password must contain at least one lowercase letter"
    );

    // Missing digit
    let result = validate_password("NoDigitsHere!");
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().to_string(),
        "Password must contain at least one digit"
    );

    // Missing special character
    let result = validate_password("NoSpecial123");
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().to_string(),
        "Password must contain at least one special character (!@#$%^&*()_+-=[]{}|;:,.<>?)"
    );

    // Missing multiple requirements
    let result = validate_password("lowercase");
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().to_string(),
        "Password must contain at least one uppercase letter"
    );
}

#[test]
fn test_validate_password_pattern_errors() {
    // Contains "password"
    let result = validate_password("MyPassword123!");
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().to_string(),
        "Password cannot contain the word 'password'"
    );

    let result = validate_password("password123!");
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().to_string(),
        "Password cannot contain the word 'password'"
    );

    let result = validate_password("Password123!");
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().to_string(),
        "Password cannot contain the word 'password'"
    );

    let result = validate_password("MyPassWord123!");
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().to_string(),
        "Password cannot contain the word 'password'"
    );

    // Sequential characters (4+ chars)
    let result = validate_password("Testabcde123!");
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().to_string(),
        "Password cannot contain sequential characters (e.g., 'abcd', '1234')"
    );

    let result = validate_password("Test12345!");
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().to_string(),
        "Password cannot contain sequential characters (e.g., 'abcd', '1234')"
    );

    let result = validate_password("Testmnop123!");
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().to_string(),
        "Password cannot contain sequential characters (e.g., 'abcd', '1234')"
    );

    // Repeated characters
    let result = validate_password("Testaaa123!");
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().to_string(),
        "Password cannot contain repeated characters (e.g., 'aaa', '111')"
    );

    let result = validate_password("Test111Pass!");
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().to_string(),
        "Password cannot contain repeated characters (e.g., 'aaa', '111')"
    );

    let result = validate_password("TestAAA123!");
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().to_string(),
        "Password cannot contain repeated characters (e.g., 'aaa', '111')"
    );
}

#[test]
fn test_validate_password_edge_cases() {
    // Exactly 8 characters with all requirements
    assert!(validate_password("Abc123!@").is_ok());

    // Non-sequential repeated patterns (should be valid)
    assert!(validate_password("TestAaAa1!").is_ok());
    assert!(validate_password("Test1a1a!").is_ok());

    // Non-consecutive sequential characters (should be valid)
    assert!(validate_password("TaestBcd1!").is_ok());
    assert!(validate_password("Test1a3c!").is_ok());

    // Short sequential sequences should be valid now (3 chars)
    assert!(validate_password("Test123!A").is_ok());
    assert!(validate_password("TestAbc9!").is_ok());

    // Password contains "pass" but not "password" (should be valid)
    assert!(validate_password("MyPass123!").is_ok());
    assert!(validate_password("PassThru9!").is_ok());

    // Case sensitivity for sequential check
    assert!(validate_password("TestAbC123!").is_ok()); // A, b, C are not sequential in ASCII

    // Unicode characters (should be rejected as they're not in allowed special chars)
    let result = validate_password("TestÜber123");
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().to_string(),
        "Password must contain at least one special character (!@#$%^&*()_+-=[]{}|;:,.<>?)"
    );
}
