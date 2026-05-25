/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub fn parse(spec: &str) -> Result<Vec<String>, String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err("empty flake reference".to_string());
    }
    let mut out = Vec::new();
    for part in spec.split(',') {
        let attr = part.trim().trim_start_matches('#').trim();
        if attr.is_empty() {
            return Err(format!("empty attribute in '{}'", spec));
        }
        out.push(attr.to_string());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_attr_with_hash() {
        assert_eq!(parse("#foo.bar"), Ok(vec!["foo.bar".to_string()]));
    }

    #[test]
    fn parse_single_attr_without_hash() {
        assert_eq!(parse("foo.bar"), Ok(vec!["foo.bar".to_string()]));
    }

    #[test]
    fn parse_multiple_attrs_comma_separated() {
        assert_eq!(
            parse("#a,#b,c"),
            Ok(vec!["a".into(), "b".into(), "c".into()])
        );
    }

    #[test]
    fn parse_strips_whitespace() {
        assert_eq!(parse(" #a , #b "), Ok(vec!["a".into(), "b".into()]));
    }

    #[test]
    fn parse_rejects_empty_token() {
        assert!(parse("#a,,#b").is_err());
    }

    #[test]
    fn parse_rejects_empty_input() {
        assert!(parse("").is_err());
        assert!(parse("   ").is_err());
    }
}
