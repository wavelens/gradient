/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result as AnyhowResult;
use std::net::{SocketAddr, ToSocketAddrs};
use thiserror::Error;

use crate::consts::*;

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
    #[error("Invalid hex string")]
    InvalidHexString,
    #[error("Commit hash must be 40 characters long")]
    InvalidCommitHashLength,
    #[error("URLs pointing to local files are not allowed")]
    LocalFileUrlNotAllowed,
    #[error("Invalid repository URL")]
    InvalidRepositoryUrl,
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
    #[error("Evaluation wildcard pattern cannot be a bare special character (`!`, `*`, or `#`)")]
    EvaluationWildcardBareSpecialChar,
    #[error("Exclusion patterns cannot contain wildcard segments (`*` or `#`)")]
    EvaluationWildcardExclusionWildcard,
    #[error("Name cannot be empty")]
    NameEmpty,
    #[error("Name must be lowercase")]
    NameNotLowercase,
    #[error("Name can only contain letters, numbers, and dashes")]
    NameInvalidCharacters,
    #[error("Name can only start and end with letters or numbers")]
    NameInvalidStartEnd,
    #[error("This task name is reserved and cannot be used")]
    TaskNameReserved,
    #[error("Empty path in manifest")]
    ManifestPathEmpty,
    #[error("Invalid path (null byte): {0}")]
    ManifestPathNullByte(String),
    #[error("Absolute path: {0}")]
    ManifestPathAbsolute(String),
    #[error("Invalid path component in: {0}")]
    ManifestPathInvalidComponent(String),
    #[error("Hash must be 64-char lowercase hex: {0}")]
    Blake3HashInvalid(String),
    #[error("input_name must match ^[A-Za-z_][A-Za-z0-9_-]*$")]
    FlakeInputNameInvalid,
    #[error(
        "override-input '{input_name}': '{flake_ref}' is not a remote flake ref; use github:/gitlab:/git+ssh://flake:/path:/nix/store/... ."
    )]
    FlakeRefNotRemote {
        input_name: String,
        flake_ref: String,
    },
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
    #[error(
        "Display name can only contain letters, numbers, spaces, apostrophes, dots, dashes, and underscores"
    )]
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

pub fn hex_to_vec(s: &str) -> Result<Vec<u8>, InputError> {
    if !s.len().is_multiple_of(2) {
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
    if url.starts_with("git+ssh://") || url.starts_with("ssh://") {
        return true;
    }

    if let Some(at_pos) = url.find('@')
        && let Some(colon_pos) = url[at_pos..].find(':')
    {
        let colon_abs_pos = at_pos + colon_pos;
        return colon_abs_pos > at_pos && !url[..at_pos].contains("://");
    }

    false
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

pub const RESERVED_TASK_NAMES: &[&str] = &["build-request"];

pub fn check_task_name(s: &str) -> Result<(), InputError> {
    check_index_name(s)?;
    if RESERVED_TASK_NAMES
        .iter()
        .any(|r| s.eq_ignore_ascii_case(r))
    {
        return Err(InputError::TaskNameReserved);
    }
    Ok(())
}

/// Startup callers must propagate the error to `init_state` for a clean exit. Per-request
/// callers must map it to a 5xx. A transient filesystem hiccup must never tear down the
/// running server.
pub fn load_secret(f: &str) -> anyhow::Result<super::secret::SecretString> {
    let s = std::fs::read_to_string(f)
        .map_err(|e| anyhow::anyhow!("Failed to read secret file '{}': {}", f, e))?;

    let cleaned = s.trim().replace(char::from(25), "");

    if cleaned.is_empty() {
        anyhow::bail!("Secret file '{}' is empty or contains only whitespace", f);
    }

    Ok(super::secret::SecretString::new(cleaned))
}

/// Plain text passwords are used as-is. Content shorter than 16 bytes is falling back to
/// base64 decoding for backwards compatibility.
pub fn load_secret_bytes(f: &str) -> anyhow::Result<super::secret::SecretBytes> {
    use base64::{Engine, engine::general_purpose};

    let secret_str = load_secret(f)?;
    let as_bytes = secret_str.expose().as_bytes();

    if as_bytes.len() >= 16 {
        return Ok(super::secret::SecretBytes::new(as_bytes.to_vec()));
    }

    match general_purpose::STANDARD.decode(secret_str.expose()) {
        Ok(decoded) if decoded.len() >= 16 => Ok(super::secret::SecretBytes::new(decoded)),
        Ok(decoded) => anyhow::bail!(
            "Secret file '{}' decoded from base64 but is too short ({} bytes). Secrets should be at least 16 bytes.",
            f,
            decoded.len()
        ),
        Err(_) => anyhow::bail!(
            "Secret file '{}' is too short ({} bytes). Secrets should be at least 16 bytes as plain text, or base64-encoded.",
            f,
            as_bytes.len()
        ),
    }
}

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

    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(InputError::UsernameInvalidCharacters);
    }

    if username.starts_with('_')
        || username.starts_with('-')
        || username.ends_with('_')
        || username.ends_with('-')
    {
        return Err(InputError::UsernameInvalidStartEnd);
    }

    if username.contains("__")
        || username.contains("--")
        || username.contains("_-")
        || username.contains("-_")
    {
        return Err(InputError::UsernameConsecutiveSpecialChars);
    }

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

    if !display_name
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '\'' | '.' | '-' | '_'))
    {
        return Err(InputError::DisplayNameInvalidCharacters);
    }

    if display_name.starts_with(' ') || display_name.ends_with(' ') {
        return Err(InputError::DisplayNameInvalidStartEnd);
    }

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

    if password.chars().collect::<Vec<_>>().windows(4).any(|w| {
        w[0] as u8 + 1 == w[1] as u8 && w[1] as u8 + 1 == w[2] as u8 && w[2] as u8 + 1 == w[3] as u8
    }) {
        return Err(InputError::PasswordSequentialChars);
    }

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

const REMOTE_FLAKE_REF_PREFIXES: &[&str] = &[
    "github:",
    "gitlab:",
    "sourcehut:",
    "git+ssh://",
    "git+https://",
    "git+http://",
    "git://",
    "https://",
    "http://",
    "flake:",
    "path:/nix/store/",
];

pub fn check_flake_input_override(input_name: &str, flake_ref: &str) -> Result<(), InputError> {
    let mut chars = input_name.chars();
    let name_ok = matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');

    if !name_ok {
        return Err(InputError::FlakeInputNameInvalid);
    }

    let is_remote = REMOTE_FLAKE_REF_PREFIXES
        .iter()
        .any(|prefix| flake_ref.starts_with(prefix));

    if !is_remote {
        return Err(InputError::FlakeRefNotRemote {
            input_name: input_name.to_string(),
            flake_ref: flake_ref.to_string(),
        });
    }

    Ok(())
}

pub fn check_manifest_path(path: &str) -> Result<(), InputError> {
    if path.is_empty() {
        return Err(InputError::ManifestPathEmpty);
    }

    if path.contains('\0') {
        return Err(InputError::ManifestPathNullByte(path.to_string()));
    }

    if path.starts_with('/') {
        return Err(InputError::ManifestPathAbsolute(path.to_string()));
    }

    let has_invalid_component = path
        .split('/')
        .any(|segment| matches!(segment, "" | "." | ".."));

    if has_invalid_component {
        return Err(InputError::ManifestPathInvalidComponent(path.to_string()));
    }

    Ok(())
}

pub fn decode_blake3_hex(hash: &str) -> Result<Vec<u8>, InputError> {
    let is_lowercase_hex = hash
        .chars()
        .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));

    if hash.len() != 64 || !is_lowercase_hex {
        return Err(InputError::Blake3HashInvalid(hash.to_string()));
    }

    hex_to_vec(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_remote_flake_refs() {
        for url in [
            "github:NixOS/nixpkgs",
            "gitlab:group/task",
            "sourcehut:~user/task",
            "git+ssh://git@h/x.git",
            "git+https://h/x.git",
            "git+http://h/x.git",
            "git://h/x.git",
            "https://h/x.tar.gz",
            "http://h/x.tar.gz",
            "flake:nixpkgs",
            "path:/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-src",
        ] {
            assert!(check_flake_input_override("nixpkgs", url).is_ok(), "{url}");
        }
    }

    #[test]
    fn rejects_local_and_non_remote_refs() {
        for url in ["/abs", "./rel", "~/x", "name", "path:/home/u/x"] {
            assert!(check_flake_input_override("nixpkgs", url).is_err(), "{url}");
        }
    }

    #[test]
    fn rejects_invalid_input_name() {
        assert!(check_flake_input_override("1bad", "github:a/b").is_err());
        assert!(check_flake_input_override("has space", "github:a/b").is_err());
        assert!(check_flake_input_override("", "github:a/b").is_err());
        assert!(check_flake_input_override("ok_name-1", "github:a/b").is_ok());
    }

    #[test]
    fn rejects_path_traversal() {
        assert!(check_manifest_path("../escape").is_err());
        assert!(check_manifest_path("foo/../bar").is_err());
        assert!(check_manifest_path("a/./b").is_err());
    }

    #[test]
    fn rejects_absolute_and_empty_paths() {
        assert!(check_manifest_path("/absolute").is_err());
        assert!(check_manifest_path("").is_err());
        assert!(check_manifest_path(".").is_err());
    }

    #[test]
    fn rejects_null_bytes() {
        assert!(check_manifest_path("with\0null").is_err());
    }

    #[test]
    fn accepts_normal_nested_paths() {
        assert!(check_manifest_path("flake.nix").is_ok());
        assert!(check_manifest_path("src/main.rs").is_ok());
        assert!(check_manifest_path("a/b/c/d.txt").is_ok());
    }

    #[test]
    fn hash_must_be_64_lowercase_hex() {
        let ok = "a".repeat(64);
        assert!(decode_blake3_hex(&ok).is_ok());
        let too_short = "a".repeat(63);
        assert!(decode_blake3_hex(&too_short).is_err());
        let uppercase = "A".repeat(64);
        assert!(decode_blake3_hex(&uppercase).is_err());
        let non_hex = "g".repeat(64);
        assert!(decode_blake3_hex(&non_hex).is_err());
    }
}
