/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::fmt;
use std::str::FromStr;

use crate::input::InputError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wildcard {
    patterns: Vec<String>,
}

impl Wildcard {
    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    pub fn get_eval_str(&self) -> String {
        let mut includes: Vec<String> = Vec::new();
        let mut excludes: Vec<String> = Vec::new();

        for pattern in &self.patterns {
            let (is_exclude, path) = match pattern.strip_prefix('!') {
                Some(body) => (true, body),
                None => (false, pattern.as_str()),
            };

            let nix_list = path_to_nix_list(path);
            if is_exclude {
                excludes.push(nix_list);
            } else {
                includes.push(nix_list);
            }
        }

        format!(
            "{{ \"include\" = [ {} ]; \"exclude\" = [ {} ]; }}",
            includes.join(" "),
            excludes.join(" "),
        )
    }

    pub fn matches(&self, attr: &str) -> bool {
        if attr.is_empty() {
            return false;
        }

        let path = unquote_segments(attr);
        let mut included = false;

        for pattern in &self.patterns {
            match pattern.strip_prefix('!') {
                Some(body) => {
                    if unquote_segments(body) == path {
                        return false;
                    }
                }
                None => included = included || pattern_covers(pattern, &path),
            }
        }

        included
    }
}

enum Seg {
    Star,
    Hash,
    Lit(String),
}

fn unquote_segments(path: &str) -> Vec<String> {
    split_segments(path)
        .into_iter()
        .map(|(seg, is_quoted)| {
            if is_quoted {
                seg.trim_matches('"').to_string()
            } else {
                seg
            }
        })
        .collect()
}

fn pattern_segments(body: &str) -> Vec<Seg> {
    let mut out: Vec<Seg> = Vec::new();

    for (seg, is_quoted) in split_segments(body) {
        let next = if is_quoted {
            Seg::Lit(seg.trim_matches('"').to_string())
        } else {
            match seg.as_str() {
                "*" => Seg::Star,
                "#" => Seg::Hash,
                _ => Seg::Lit(seg),
            }
        };

        if matches!(next, Seg::Star) && matches!(out.last(), Some(Seg::Star)) {
            continue;
        }

        out.push(next);
    }

    out
}

/// A trailing `*` is matching one or two path elements. The evaluator is descending one extra
/// level there. Every other segment is matching exactly one element.
fn pattern_covers(pattern: &str, path: &[String]) -> bool {
    let segs = pattern_segments(pattern);
    let Some(last) = segs.last() else {
        return false;
    };

    if matches!(last, Seg::Star) {
        if path.len() != segs.len() && path.len() != segs.len() + 1 {
            return false;
        }
    } else if path.len() != segs.len() {
        return false;
    }

    segs.iter().enumerate().all(|(i, seg)| match seg {
        Seg::Star | Seg::Hash => true,
        Seg::Lit(lit) => path[i] == *lit,
    })
}

fn split_segments(pattern: &str) -> Vec<(String, bool)> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut seg_is_quoted = false;

    for ch in pattern.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                if in_quotes {
                    seg_is_quoted = true;
                }
                current.push(ch);
            }
            '.' if !in_quotes => {
                segments.push((std::mem::take(&mut current), seg_is_quoted));
                seg_is_quoted = false;
            }
            _ => current.push(ch),
        }
    }
    segments.push((current, seg_is_quoted));
    segments
}

fn path_to_nix_list(path: &str) -> String {
    let raw_elems: Vec<String> = split_segments(path)
        .into_iter()
        .map(|(seg, is_quoted)| {
            let content = if is_quoted {
                seg.trim_matches('"').to_string()
            } else {
                seg
            };
            format!("\"{}\"", content)
        })
        .collect();

    // `*.*` is identical to `*` because `*` is recursive. `#` is not recursive.
    // Each `#` is targeting its own depth level and must stay uncollapsed.
    let mut elems: Vec<String> = Vec::new();
    for elem in raw_elems {
        if elem == "\"*\"" && elems.last().is_some_and(|l| l == "\"*\"") {
            continue;
        }
        elems.push(elem);
    }

    format!("[ {} ]", elems.join(" "))
}

fn validate_segments(path: &str) -> Result<(), InputError> {
    for (seg, is_quoted) in split_segments(path) {
        if is_quoted {
            let inner = seg.trim_matches('"');
            if matches!(inner, "*" | "#" | "!") {
                return Err(InputError::EvaluationWildcardBareSpecialChar);
            }
        } else if seg.starts_with('!') {
            return Err(InputError::EvaluationWildcardBareSpecialChar);
        }
    }
    Ok(())
}

fn validate_pattern(part: &str) -> Result<(), InputError> {
    if part.is_empty() {
        return Err(InputError::EvaluationWildcardEmpty);
    }
    if part.split_whitespace().count() > 1 {
        return Err(InputError::EvaluationWildcardInternalWhitespace);
    }

    let (is_exclusion, body) = match part.strip_prefix('!') {
        Some(b) => (true, b),
        None => (false, part),
    };

    if body.is_empty() {
        return Err(InputError::EvaluationWildcardBareSpecialChar);
    }

    if body.starts_with('.') {
        return Err(InputError::EvaluationWildcardStartsWithPeriod);
    }

    if body == "#" {
        return Err(InputError::EvaluationWildcardBareSpecialChar);
    }

    if is_exclusion {
        for (seg, is_quoted) in split_segments(body) {
            if !is_quoted && matches!(seg.as_str(), "*" | "#") {
                return Err(InputError::EvaluationWildcardExclusionWildcard);
            }
        }
    }

    validate_segments(body)
}

impl FromStr for Wildcard {
    type Err = InputError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.trim() != s {
            return Err(InputError::EvaluationWildcardWhitespace);
        }
        if s.contains(",,") {
            return Err(InputError::EvaluationWildcardConsecutiveCommas);
        }

        let mut patterns = Vec::new();

        for part in s.split(',').map(|p| p.trim()) {
            validate_pattern(part)?;
            patterns.push(part.to_string());
        }

        Ok(Self { patterns })
    }
}

impl fmt::Display for Wildcard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.patterns.join(","))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiple_patterns() {
        let w: Wildcard = "packages.*.*,checks.*.*".parse().unwrap();
        assert_eq!(w.patterns(), &["packages.*.*", "checks.*.*"]);
        assert_eq!(w.to_string(), "packages.*.*,checks.*.*");
    }

    #[test]
    fn trims_spaces_between_patterns() {
        let w: Wildcard = "packages.*.*, checks.*.*".parse().unwrap();
        assert_eq!(w.patterns(), &["packages.*.*", "checks.*.*"]);
        assert_eq!(w.to_string(), "packages.*.*,checks.*.*");
    }

    #[test]
    fn quoted_segment_with_dot_valid() {
        let w: Wildcard = r#"my."wild.card".is.*"#.parse().unwrap();
        assert_eq!(w.patterns(), &[r#"my."wild.card".is.*"#]);
        assert_eq!(w.to_string(), r#"my."wild.card".is.*"#);
    }

    #[test]
    fn exclusion_with_wildcard_rejected() {
        assert!("my.*,!my.ignored.*".parse::<Wildcard>().is_err());
    }

    #[test]
    fn exclusion_with_hash_rejected() {
        assert!(
            "packages.*.*,!packages.x86_64-linux.#"
                .parse::<Wildcard>()
                .is_err()
        );
    }

    #[test]
    fn exclusion_with_quoted_segment_valid() {
        let w: Wildcard = r#"packages.*.*,!packages.x86_64-linux."broken.pkg""#
            .parse()
            .unwrap();
        assert_eq!(w.patterns().len(), 2);
    }

    #[test]
    fn roundtrip() {
        let original = r#"packages.*.*,!packages.x86_64-linux.broken,my."wild.card".*"#;
        let w: Wildcard = original.parse().unwrap();
        assert_eq!(w.to_string(), original);
    }

    #[test]
    fn eval_str_include_only() {
        let w: Wildcard = "packages.*.*".parse().unwrap();
        assert_eq!(
            w.get_eval_str(),
            r#"{ "include" = [ [ "packages" "*" ] ]; "exclude" = [  ]; }"#,
        );
    }

    #[test]
    fn eval_str_bare_star() {
        let w: Wildcard = "*".parse().unwrap();
        assert_eq!(
            w.get_eval_str(),
            r#"{ "include" = [ [ "*" ] ]; "exclude" = [  ]; }"#,
        );
    }

    #[test]
    fn eval_str_include_and_exclude() {
        let w: Wildcard = "packages.*.*,!packages.x86_64-linux.broken"
            .parse()
            .unwrap();
        assert_eq!(
            w.get_eval_str(),
            r#"{ "include" = [ [ "packages" "*" ] ]; "exclude" = [ [ "packages" "x86_64-linux" "broken" ] ]; }"#,
        );
    }

    #[test]
    fn eval_str_quoted_segment_unwrapped() {
        let w: Wildcard = r#"my."wild.card".*"#.parse().unwrap();
        assert_eq!(
            w.get_eval_str(),
            r#"{ "include" = [ [ "my" "wild.card" "*" ] ]; "exclude" = [  ]; }"#,
        );
    }

    #[test]
    fn eval_str_multiple_includes() {
        let w: Wildcard = "packages.*.*.*,checks.*".parse().unwrap();
        assert_eq!(
            w.get_eval_str(),
            r#"{ "include" = [ [ "packages" "*" ] [ "checks" "*" ] ]; "exclude" = [  ]; }"#,
        );
    }

    #[test]
    fn bare_hash_rejected() {
        assert!("#".parse::<Wildcard>().is_err());
    }

    #[test]
    fn bare_exclamation_rejected() {
        assert!("!".parse::<Wildcard>().is_err());
    }

    #[test]
    fn mid_path_exclamation_rejected() {
        assert!("my.!ignored".parse::<Wildcard>().is_err());
        assert!("my.!*".parse::<Wildcard>().is_err());
    }

    #[test]
    fn quoted_star_segment_rejected() {
        assert!(r#"my."*".not.allowed.*"#.parse::<Wildcard>().is_err());
    }

    #[test]
    fn quoted_hash_segment_rejected() {
        assert!(r##"my."#".something"##.parse::<Wildcard>().is_err());
    }

    #[test]
    fn quoted_exclamation_segment_rejected() {
        assert!(r#"my."!".something"#.parse::<Wildcard>().is_err());
    }

    #[test]
    fn quoted_star_in_exclusion_rejected() {
        assert!(r#"packages.*.*,!my."*".foo"#.parse::<Wildcard>().is_err());
    }

    #[test]
    fn empty_rejected() {
        assert!("".parse::<Wildcard>().is_err());
    }

    #[test]
    fn double_comma_rejected() {
        assert_eq!(
            "packages.*.*,,checks.*.*".parse::<Wildcard>().unwrap_err(),
            InputError::EvaluationWildcardConsecutiveCommas,
        );
    }

    #[test]
    fn leading_space_rejected() {
        assert_eq!(
            " packages.*.*".parse::<Wildcard>().unwrap_err(),
            InputError::EvaluationWildcardWhitespace,
        );
    }

    #[test]
    fn trailing_space_rejected() {
        assert_eq!(
            "packages.*.* ".parse::<Wildcard>().unwrap_err(),
            InputError::EvaluationWildcardWhitespace,
        );
    }

    #[test]
    fn internal_whitespace_rejected() {
        assert_eq!(
            "packages .*.*".parse::<Wildcard>().unwrap_err(),
            InputError::EvaluationWildcardInternalWhitespace,
        );
    }

    #[test]
    fn starts_with_period_rejected() {
        assert_eq!(
            ".packages.*.*".parse::<Wildcard>().unwrap_err(),
            InputError::EvaluationWildcardStartsWithPeriod,
        );
    }

    #[test]
    fn exclusion_bare_body_rejected() {
        assert_eq!(
            "packages.*.*,!".parse::<Wildcard>().unwrap_err(),
            InputError::EvaluationWildcardBareSpecialChar,
        );
    }

    #[test]
    fn exclusion_starts_with_period_rejected() {
        assert_eq!(
            "packages.*.*,!.packages".parse::<Wildcard>().unwrap_err(),
            InputError::EvaluationWildcardStartsWithPeriod,
        );
    }
}

#[cfg(test)]
mod matches_tests {
    use super::*;

    fn m(pattern: &str, attr: &str) -> bool {
        pattern.parse::<Wildcard>().unwrap().matches(attr)
    }

    #[test]
    fn exact_path_matches_itself_only() {
        assert!(m(
            "packages.x86_64-linux.hello",
            "packages.x86_64-linux.hello"
        ));
        assert!(!m(
            "packages.x86_64-linux.hello",
            "packages.x86_64-linux.world"
        ));
        assert!(!m("packages.x86_64-linux.hello", "packages.x86_64-linux"));
    }

    #[test]
    fn star_mid_pattern_matches_exactly_one_segment() {
        assert!(m("my.*.test", "my.foo.test"));
        assert!(!m("my.*.test", "my.test"));
        assert!(!m("my.*.test", "my.foo.bar.test"));
    }

    #[test]
    fn hash_matches_exactly_one_segment_and_does_not_descend() {
        assert!(m("packages.x86_64-linux.#", "packages.x86_64-linux.hello"));
        assert!(!m(
            "packages.x86_64-linux.#",
            "packages.x86_64-linux.py.hello"
        ));
        assert!(!m("packages.x86_64-linux.#", "packages.x86_64-linux"));
    }

    #[test]
    fn trailing_star_matches_one_or_two_segments() {
        assert!(m("packages.*", "packages.x86_64-linux"));
        assert!(m("packages.*", "packages.x86_64-linux.hello"));
        assert!(!m("packages.*", "packages"));
        assert!(!m("packages.*", "packages.x86_64-linux.py.hello"));
    }

    #[test]
    fn consecutive_stars_collapse() {
        for attr in ["packages.x86_64-linux", "packages.x86_64-linux.hello"] {
            assert_eq!(m("packages.*.*", attr), m("packages.*", attr), "{attr}");
        }
        assert!(m("packages.*.*", "packages.x86_64-linux.hello"));
        assert!(!m("packages.*.*", "packages.x86_64-linux.py.hello"));
    }

    #[test]
    fn bare_star_matches_top_two_levels() {
        assert!(m("*", "hello"));
        assert!(m("*", "packages.hello"));
        assert!(!m("*", "packages.x86_64-linux.hello"));
    }

    #[test]
    fn any_include_pattern_may_match() {
        let w: Wildcard = "packages.*.*,checks.*.*".parse().unwrap();
        assert!(w.matches("packages.x86_64-linux.hello"));
        assert!(w.matches("checks.x86_64-linux.fmt"));
        assert!(!w.matches("devShells.x86_64-linux.default"));
    }

    #[test]
    fn exclusion_removes_an_otherwise_matching_path() {
        let w: Wildcard = "packages.*.*,!packages.x86_64-linux.broken"
            .parse()
            .unwrap();
        assert!(w.matches("packages.x86_64-linux.hello"));
        assert!(!w.matches("packages.x86_64-linux.broken"));
        assert!(w.matches("packages.aarch64-linux.broken"));
    }

    #[test]
    fn quoted_segments_compare_by_inner_content() {
        assert!(m(
            r#"packages.*."python3.12""#,
            r#"packages.x86_64-linux."python3.12""#
        ));
        assert!(!m(
            r#"packages.*."python3.12""#,
            "packages.x86_64-linux.python3.12"
        ));
        assert!(m(r#"my."wild.card".test"#, r#"my."wild.card".test"#));
        assert!(!m(r#"my."wild.card".test"#, "my.wild.card.test"));
    }

    #[test]
    fn quoted_attr_segments_are_unwrapped_too() {
        assert!(m("packages.*.*", r#"packages."x86_64-linux".hello"#));
    }

    #[test]
    fn empty_attr_never_matches() {
        assert!(!m("*", ""));
        assert!(!m("packages.*.*", ""));
    }
}
