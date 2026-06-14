/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

/// Parsed SCIM filter: only `attr eq "value"` is supported (the subset Okta and
/// Entra emit for sync). Returns `(attribute_lowercased, value)`.
pub fn parse_eq_filter(filter: &str) -> Option<(String, String)> {
    let mut parts = filter.splitn(3, char::is_whitespace);
    let attr = parts.next()?.trim();
    let op = parts.next()?.trim();
    let value = parts.next()?.trim();
    if !op.eq_ignore_ascii_case("eq") {
        return None;
    }

    let value = value.trim_matches('"').to_string();
    Some((attr.to_ascii_lowercase(), value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_username_eq() {
        let (attr, val) = parse_eq_filter(r#"userName eq "alice@example.com""#).unwrap();
        assert_eq!(attr, "username");
        assert_eq!(val, "alice@example.com");
    }

    #[test]
    fn rejects_non_eq() {
        assert!(parse_eq_filter(r#"userName co "ali""#).is_none());
    }
}
