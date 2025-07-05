/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::net::{SocketAddr, ToSocketAddrs};

use super::consts::*;

pub fn url_to_addr(host: &str, port: i32) -> Result<SocketAddr, Box<dyn std::error::Error>> {
    let port = port as usize;

    if !PORT_RANGE.contains(&port) {
        return Err(format!(
            "port out of range {}-{}",
            PORT_RANGE.start(),
            PORT_RANGE.end()
        )
        .into());
    }

    let uri = format!("{}:{}", host, port);
    let url = uri
        .to_socket_addrs()?
        .next()
        .ok_or(format!("{} is not a valid address", uri))?;
    Ok(url)
}

pub fn port_in_range(s: &str) -> Result<u16, String> {
    let port: usize = s
        .parse()
        .map_err(|_| format!("`{s}` is not a port number"))?;

    if PORT_RANGE.contains(&port) {
        Ok(port as u16)
    } else {
        Err(format!(
            "port not in range {}-{}",
            PORT_RANGE.start(),
            PORT_RANGE.end()
        ))
    }
}

pub fn greater_than_zero<
    T: std::str::FromStr + std::cmp::PartialOrd + std::fmt::Display + Default,
>(
    s: &str,
) -> Result<T, String> {
    let num: T = s
        .parse()
        .map_err(|_| format!("`{}` is not a valid number", s))?;

    if num > T::default() {
        Ok(num)
    } else {
        Err(format!("`{}` is not larger than 0", s))
    }
}

pub fn hex_to_vec(s: &str) -> Result<Vec<u8>, String> {
    if s.len() % 2 != 0 {
        return Err("invalid hex string".to_string());
    }

    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

pub fn vec_to_hex(v: &[u8]) -> String {
    v.iter().map(|b| format!("{:02x}", b)).collect()
}

pub fn repository_url_to_nix(url: &str, commit_hash: &str) -> Result<String, String> {
    if commit_hash.len() != 40 {
        return Err("commit hash must be 40 characters long".to_string());
    }

    if url.contains("file://") || url.starts_with("file") {
        return Err("URLs pointing to local files are not allowed".to_string());
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
    url.starts_with("git+ssh://")
}

pub fn parse_evaluation_wildcard(s: &str) -> Result<Vec<&str>, String> {
    if s.trim() != s {
        return Err("Evaluation wildcard cannot have leading or trailing whitespace".to_string());
    } else if s.contains(",,") {
        return Err("Evaluation wildcard cannot have consecutive commas".to_string());
    } else if s.split_whitespace().count() > 1 {
        return Err("Evaluation wildcard cannot have whitespace".to_string());
    }

    let seperate_evaluations = s.split(",").map(|sub| sub.trim()).collect::<Vec<&str>>();

    let mut evaluations = Vec::new();

    for evaluation in seperate_evaluations {
        if evaluation.is_empty() {
            return Err("Evaluation wildcard cannot be empty".to_string());
        }

        if evaluation.starts_with(".") {
            return Err("Evaluation wildcard cannot start with a period".to_string());
        }

        evaluations.push(evaluation);
    }

    if evaluations.is_empty() {
        return Err("Evaluation wildcard cannot be empty".to_string());
    }

    Ok(evaluations)
}

pub fn valid_evaluation_wildcard(s: &str) -> bool {
    parse_evaluation_wildcard(s).is_ok()
}

pub fn check_index_name(s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err("Name cannot be empty".to_string());
    }

    if s != s.to_lowercase() {
        return Err("Name must be lowercase".to_string());
    }

    if s.contains(|c: char| !c.is_ascii_alphanumeric() && c != '-') {
        return Err("Name can only contain letters, numbers, and dashes".to_string());
    }

    if s.starts_with('-') || s.ends_with('-') {
        return Err("Name can only start and end with letters or numbers".to_string());
    }

    Ok(())
}

pub fn load_secret(f: &str) -> String {
    let s = std::fs::read_to_string(f).unwrap_or_default();
    s.trim().replace(char::from(25), "")
}

/// Validates password strength requirements
pub fn validate_password(password: &str) -> Result<(), String> {
    if password.len() < 8 {
        return Err("Password must be at least 8 characters long".to_string());
    }
    
    if password.len() > 128 {
        return Err("Password cannot exceed 128 characters".to_string());
    }
    
    // Check for common patterns first
    if password.to_lowercase().contains("password") {
        return Err("Password cannot contain the word 'password'".to_string());
    }
    
    let has_uppercase = password.chars().any(|c| c.is_uppercase());
    let has_lowercase = password.chars().any(|c| c.is_lowercase());
    let has_digit = password.chars().any(|c| c.is_ascii_digit());
    let has_special = password.chars().any(|c| "!@#$%^&*()_+-=[]{}|;:,.<>?".contains(c));
    
    if !has_uppercase {
        return Err("Password must contain at least one uppercase letter".to_string());
    }
    
    if !has_lowercase {
        return Err("Password must contain at least one lowercase letter".to_string());
    }
    
    if !has_digit {
        return Err("Password must contain at least one digit".to_string());
    }
    
    if !has_special {
        return Err("Password must contain at least one special character (!@#$%^&*()_+-=[]{}|;:,.<>?)".to_string());
    }
    
    // Check for common weak sequences (4+ characters)
    if password.chars().collect::<Vec<_>>().windows(4).any(|w| {
        w[0] as u8 + 1 == w[1] as u8 && w[1] as u8 + 1 == w[2] as u8 && w[2] as u8 + 1 == w[3] as u8
    }) {
        return Err("Password cannot contain sequential characters (e.g., 'abcd', '1234')".to_string());
    }
    
    // Check for repeated characters (3+ in a row)
    if password.chars().collect::<Vec<_>>().windows(3).any(|w| {
        w[0] == w[1] && w[1] == w[2]
    }) {
        return Err("Password cannot contain repeated characters (e.g., 'aaa', '111')".to_string());
    }
    
    Ok(())
}
