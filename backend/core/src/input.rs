/*
 * SPDX-FileCopyrightText: 2025 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result as AnyhowResult;
use std::net::{SocketAddr, ToSocketAddrs};
use thiserror::Error;

use super::consts::*;

#[derive(Debug, Error, PartialEq)]
pub enum InputError {
    #[error("Port {port} is out of range {start}-{end}", port = .0, start = PORT_RANGE.start(), end = PORT_RANGE.end())]
    PortOutOfRange(i32),
    #[error("Invalid address: {0}")]
    InvalidAddress(String),
    #[error("`{0}` is not a port number")]
    InvalidPortNumber(String),
    #[error("Port not in range {start}-{end}", start = PORT_RANGE.start(), end = PORT_RANGE.end())]
    PortNotInRange,
    #[error("`{0}` is not a valid number")]
    InvalidNumber(String),
    #[error("`{0}` is not larger than 0")]
    NotGreaterThanZero(String),
    #[error("Invalid hex string")]
    InvalidHexString,
    #[error("Commit hash must be 40 characters long")]
    InvalidCommitHashLength,
    #[error("URLs pointing to local files are not allowed")]
    LocalFileUrlNotAllowed,
    #[error("Evaluation wildcard cannot have leading or trailing whitespace")]
    EvaluationWildcardWhitespace,
    #[error("Evaluation wildcard cannot have consecutive commas")]
    EvaluationWildcardConsecutiveCommas,
    #[error("Evaluation wildcard cannot have whitespace")]
    EvaluationWildcardInternalWhitespace,
    #[error("Evaluation wildcard cannot be empty")]
    EvaluationWildcardEmpty,
    #[error("Evaluation wildcard cannot start with a period")]
    EvaluationWildcardStartsWithPeriod,
    #[error("Name cannot be empty")]
    NameEmpty,
    #[error("Name must be lowercase")]
    NameNotLowercase,
    #[error("Name can only contain letters, numbers, and dashes")]
    NameInvalidCharacters,
    #[error("Name can only start and end with letters or numbers")]
    NameInvalidStartEnd,
    #[error("Username cannot be empty")]
    UsernameEmpty,
    #[error("Username must be at least 3 characters long")]
    UsernameTooShort,
    #[error("Username cannot exceed 50 characters")]
    UsernameTooLong,
    #[error("Username can only contain letters, numbers, underscores, and hyphens")]
    UsernameInvalidCharacters,
    #[error("Username cannot start or end with underscore or hyphen")]
    UsernameInvalidStartEnd,
    #[error("Username cannot contain consecutive special characters")]
    UsernameConsecutiveSpecialChars,
    #[error("This username is reserved and cannot be used")]
    UsernameReserved,
    #[error("Display name cannot be empty")]
    DisplayNameEmpty,
    #[error("Display name cannot exceed 100 characters")]
    DisplayNameTooLong,
    #[error("Display name can only contain letters, numbers, and spaces")]
    DisplayNameInvalidCharacters,
    #[error("Display name cannot start or end with spaces")]
    DisplayNameInvalidStartEnd,
    #[error("Display name cannot contain consecutive spaces")]
    DisplayNameConsecutiveSpaces,
    #[error("Password must be at least 8 characters long")]
    PasswordTooShort,
    #[error("Password cannot exceed 128 characters")]
    PasswordTooLong,
    #[error("Password cannot contain the word 'password'")]
    PasswordContainsPassword,
    #[error("Password must contain at least one uppercase letter")]
    PasswordMissingUppercase,
    #[error("Password must contain at least one lowercase letter")]
    PasswordMissingLowercase,
    #[error("Password must contain at least one digit")]
    PasswordMissingDigit,
    #[error("Password must contain at least one special character (!@#$%^&*()_+-=[]{{}}|;:,.<>?)")]
    PasswordMissingSpecialChar,
    #[error("Password cannot contain sequential characters (e.g., 'abcd', '1234')")]
    PasswordSequentialChars,
    #[error("Password cannot contain repeated characters (e.g., 'aaa', '111')")]
    PasswordRepeatedChars,
}

pub fn url_to_addr(host: &str, port: i32) -> AnyhowResult<SocketAddr> {
    let port_usize = port as usize;

    if !PORT_RANGE.contains(&port_usize) {
        return Err(InputError::PortOutOfRange(port).into());
    }

    let uri = format!("{}:{}", host, port);
    let url = uri
        .to_socket_addrs()?
        .next()
        .ok_or(InputError::InvalidAddress(uri))?;
    Ok(url)
}

pub fn port_in_range(s: &str) -> Result<u16, InputError> {
    let port: usize = s
        .parse()
        .map_err(|_| InputError::InvalidPortNumber(s.to_string()))?;

    if PORT_RANGE.contains(&port) {
        Ok(port as u16)
    } else {
        Err(InputError::PortNotInRange)
    }
}

pub fn greater_than_zero<
    T: std::str::FromStr + std::cmp::PartialOrd + std::fmt::Display + Default,
>(
    s: &str,
) -> Result<T, InputError> {
    let num: T = s
        .parse()
        .map_err(|_| InputError::InvalidNumber(s.to_string()))?;

    if num > T::default() {
        Ok(num)
    } else {
        Err(InputError::NotGreaterThanZero(s.to_string()))
    }
}

pub fn hex_to_vec(s: &str) -> Result<Vec<u8>, InputError> {
    if s.len() % 2 != 0 {
        return Err(InputError::InvalidHexString);
    }

    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| InputError::InvalidHexString))
        .collect()
}

pub fn vec_to_hex(v: &[u8]) -> String {
    v.iter().map(|b| format!("{:02x}", b)).collect()
}

pub fn repository_url_to_nix(url: &str, commit_hash: &str) -> Result<String, InputError> {
    if commit_hash.len() != 40 {
        return Err(InputError::InvalidCommitHashLength);
    }

    if url.contains("file://") || url.starts_with("file") {
        return Err(InputError::LocalFileUrlNotAllowed);
    }

    let url =
        if url.starts_with("ssh://") || url.starts_with("http://") || url.starts_with("https://") {
            format!("git+{}", url)
        } else {
            url.to_string()
        };

    Ok(format!("{}?rev={}", url, commit_hash))
}

pub fn check_repository_url_is_ssh(url: &str) -> bool {
    // Check for explicit SSH protocols
    if url.starts_with("git+ssh://") || url.starts_with("ssh://") {
        return true;
    }

    // Check for SCP-like syntax: user@host:path
    // This is the most common format for GitHub/GitLab (e.g., git@github.com:user/repo.git)
    if let Some(at_pos) = url.find('@') {
        if let Some(colon_pos) = url[at_pos..].find(':') {
            // Ensure the colon is not part of a protocol (e.g., not in "https://")
            let colon_abs_pos = at_pos + colon_pos;
            return colon_abs_pos > at_pos && !url[..at_pos].contains("://");
        }
    }

    false
}

pub fn parse_evaluation_wildcard(s: &str) -> Result<Vec<&str>, InputError> {
    if s.trim() != s {
        return Err(InputError::EvaluationWildcardWhitespace);
    } else if s.contains(",,") {
        return Err(InputError::EvaluationWildcardConsecutiveCommas);
    } else if s.split_whitespace().count() > 1 {
        return Err(InputError::EvaluationWildcardInternalWhitespace);
    }

    let seperate_evaluations = s.split(",").map(|sub| sub.trim()).collect::<Vec<&str>>();

    let mut evaluations = Vec::new();

    for evaluation in seperate_evaluations {
        if evaluation.is_empty() {
            return Err(InputError::EvaluationWildcardEmpty);
        }

        if evaluation.starts_with(".") {
            return Err(InputError::EvaluationWildcardStartsWithPeriod);
        }

        evaluations.push(evaluation);
    }

    if evaluations.is_empty() {
        return Err(InputError::EvaluationWildcardEmpty);
    }

    Ok(evaluations)
}

pub fn valid_evaluation_wildcard(s: &str) -> bool {
    parse_evaluation_wildcard(s).is_ok()
}

pub fn check_index_name(s: &str) -> Result<(), InputError> {
    if s.is_empty() {
        return Err(InputError::NameEmpty);
    }

    if s != s.to_lowercase() {
        return Err(InputError::NameNotLowercase);
    }

    if s.contains(|c: char| !c.is_ascii_alphanumeric() && c != '-') {
        return Err(InputError::NameInvalidCharacters);
    }

    if s.starts_with('-') || s.ends_with('-') {
        return Err(InputError::NameInvalidStartEnd);
    }

    Ok(())
}

pub fn load_secret(f: &str) -> String {
    let s = std::fs::read_to_string(f).unwrap_or_else(|e| {
        eprintln!("Failed to read secret file '{}': {}", f, e);
        std::process::exit(1);
    });

    let cleaned = s.trim().replace(char::from(25), "");

    if cleaned.is_empty() {
        eprintln!("Secret file '{}' is empty or contains only whitespace", f);
        std::process::exit(1);
    }

    cleaned
}

/// Validates password strength requirements
/// Validates username format and content requirements
pub fn validate_username(username: &str) -> Result<(), InputError> {
    if username.is_empty() {
        return Err(InputError::UsernameEmpty);
    }

    if username.len() < 3 {
        return Err(InputError::UsernameTooShort);
    }

    if username.len() > 50 {
        return Err(InputError::UsernameTooLong);
    }

    // Check for valid characters (alphanumeric, underscore, hyphen)
    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(InputError::UsernameInvalidCharacters);
    }

    // Cannot start or end with underscore or hyphen
    if username.starts_with('_')
        || username.starts_with('-')
        || username.ends_with('_')
        || username.ends_with('-')
    {
        return Err(InputError::UsernameInvalidStartEnd);
    }

    // Cannot contain consecutive underscores or hyphens
    if username.contains("__")
        || username.contains("--")
        || username.contains("_-")
        || username.contains("-_")
    {
        return Err(InputError::UsernameConsecutiveSpecialChars);
    }

    // Reserved usernames
    let reserved = [
        "admin",
        "root",
        "system",
        "api",
        "www",
        "mail",
        "ftp",
        "test",
        "user",
        "support",
        "help",
        "info",
        "null",
        "undefined",
    ];
    if reserved.contains(&username.to_lowercase().as_str()) {
        return Err(InputError::UsernameReserved);
    }

    Ok(())
}

pub fn validate_display_name(display_name: &str) -> Result<(), InputError> {
    if display_name.is_empty() {
        return Err(InputError::DisplayNameEmpty);
    }

    if display_name.len() > 100 {
        return Err(InputError::DisplayNameTooLong);
    }

    // Check for valid characters (alphanumeric and spaces only)
    if !display_name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == ' ')
    {
        return Err(InputError::DisplayNameInvalidCharacters);
    }

    // Cannot start or end with spaces
    if display_name.starts_with(' ') || display_name.ends_with(' ') {
        return Err(InputError::DisplayNameInvalidStartEnd);
    }

    // Cannot contain consecutive spaces
    if display_name.contains("  ") {
        return Err(InputError::DisplayNameConsecutiveSpaces);
    }

    Ok(())
}

pub fn validate_password(password: &str) -> Result<(), InputError> {
    if password.len() < 8 {
        return Err(InputError::PasswordTooShort);
    }

    if password.len() > 128 {
        return Err(InputError::PasswordTooLong);
    }

    // Check for common patterns first
    if password.to_lowercase().contains("password") {
        return Err(InputError::PasswordContainsPassword);
    }

    let has_uppercase = password.chars().any(|c| c.is_uppercase());
    let has_lowercase = password.chars().any(|c| c.is_lowercase());
    let has_digit = password.chars().any(|c| c.is_ascii_digit());
    let has_special = password
        .chars()
        .any(|c| "!@#$%^&*()_+-=[]{}|;:,.<>?".contains(c));

    if !has_uppercase {
        return Err(InputError::PasswordMissingUppercase);
    }

    if !has_lowercase {
        return Err(InputError::PasswordMissingLowercase);
    }

    if !has_digit {
        return Err(InputError::PasswordMissingDigit);
    }

    if !has_special {
        return Err(InputError::PasswordMissingSpecialChar);
    }

    // Check for common weak sequences (4+ characters)
    if password.chars().collect::<Vec<_>>().windows(4).any(|w| {
        w[0] as u8 + 1 == w[1] as u8 && w[1] as u8 + 1 == w[2] as u8 && w[2] as u8 + 1 == w[3] as u8
    }) {
        return Err(InputError::PasswordSequentialChars);
    }

    // Check for repeated characters (3+ in a row)
    if password
        .chars()
        .collect::<Vec<_>>()
        .windows(3)
        .any(|w| w[0] == w[1] && w[1] == w[2])
    {
        return Err(InputError::PasswordRepeatedChars);
    }

    Ok(())
}
