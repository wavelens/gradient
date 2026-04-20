/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::fmt;
use std::str::FromStr;

use crate::types::input::InputError;

/// A parsed, validated evaluation wildcard.
///
/// A wildcard is a comma-separated list of one or more Nix attribute-path
/// patterns. Each pattern is a `.`-separated sequence of segments, where
/// segments may be double-quoted to allow dots or other special characters in
/// attribute names (e.g. `my."python3.12".*`). Unquoted `*` is a wildcard
/// that matches any attribute name at that position.
///
/// Patterns prefixed with `!` are exclusions — they remove matching paths
/// from the set built by the preceding include patterns:
/// `packages.*.*,!packages.x86_64-linux.broken` includes everything in
/// `packages.*.*` except that one path.
///
/// ## Wildcard segments: `*` vs `#`
///
/// Both `*` and `#` match any attribute name at their position, but they
/// differ in how they handle the leaf level:
///
/// - `*` is **recursive**: consecutive `*` segments are collapsed before
///   evaluation (so `packages.*.*` and `packages.*` are equivalent), and when
///   a `*` is the last segment the evaluator descends one additional level into
///   nested attrsets to find derivations. This is the common case for patterns
///   like `packages.*.*` where the intermediate `*` (system) is collapsed away.
///
/// - `#` is **non-recursive**: it matches any attribute name at its position
///   and checks whether that node is a derivation (`type == "derivation"`), but
///   it does **not** descend further. Use `#` when you want to target exactly
///   the attributes at a specific depth, e.g. `packages.x86_64-linux.#`
///   collects only direct children of `packages.x86_64-linux` that are
///   derivations, without walking into nested attrsets.
///
/// `#` is rejected as a bare pattern (the whole string) and inside exclusion
/// patterns, but is valid as a segment within an include pattern.
///
/// # Rules
///
/// - No leading/trailing whitespace on the whole string.
/// - No empty patterns (consecutive commas, or trailing comma).
/// - No internal whitespace within a pattern.
/// - A pattern may start with `!` (exclusion prefix) but not `!` alone.
/// - No pattern (or exclusion body) starting with `.`.
/// - Bare `#` as the entire pattern is rejected (internal sentinel).
/// - Bare `*` as the entire pattern is valid and means "evaluate everything".
/// - Quoted segments (`"…"`) whose content is `*`, `#`, or `!` are rejected.
/// - Unquoted segments starting with `!` are rejected (`!` is only valid as a
///   whole-pattern prefix, e.g. `!packages.broken` — not `my.!bad`).
///
/// # Example
///
/// ```
/// use core::wildcard::Wildcard;
///
/// let w: Wildcard = r#"packages.*.*,!packages.x86_64-linux.broken,my."wild.card".*"#
///     .parse().unwrap();
/// assert_eq!(w.patterns().len(), 3);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wildcard {
    patterns: Vec<String>,
}

impl Wildcard {
    /// Returns the individual patterns that make up this wildcard.
    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    /// Converts the wildcard into a Nix attribute-set string suitable for
    /// passing to the evaluator.
    ///
    /// Include and exclude patterns are separated and each path is represented
    /// as a Nix list of strings, one element per segment. Quoted segments are
    /// unwrapped so only their inner content appears.
    ///
    /// # Example
    ///
    /// ```
    /// use core::wildcard::Wildcard;
    ///
    /// let w: Wildcard = r#"my."wild.card".*,my.*.test,!my.ignored.pkg"#.parse().unwrap();
    /// let s = w.get_eval_str();
    /// assert!(s.contains(r#"[ "my" "wild.card" "*" ]"#));
    /// assert!(s.contains(r#"[ "my" "*" "test" ]"#));
    /// assert!(s.contains(r#"[ "my" "ignored" "pkg" ]"#));
    /// ```
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
}

/// Splits a Nix attribute-path pattern on `.`, respecting double-quoted
/// segments. Returns each segment together with a flag indicating whether it
/// was enclosed in double quotes.
///
/// Example: `my."wild.card".*` → `[("my", false), ("\"wild.card\"", true), ("*", false)]`
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

/// Converts a path body (no leading `!`) into a Nix list-of-strings literal,
/// one element per segment. Quoted segments are unwrapped to their inner content.
///
/// Example: `my."wild.card".*` → `[ "my" "wild.card" "*" ]`
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

    // Collapse consecutive `"*"` segments — `*.*` is semantically identical to `*`
    // because `*` is recursive. `#` is non-recursive so `#.#` is NOT collapsed:
    // each `#` targets a distinct depth level.
    let mut elems: Vec<String> = Vec::new();
    for elem in raw_elems {
        if elem == "\"*\"" && elems.last().is_some_and(|l| l == "\"*\"") {
            continue;
        }
        elems.push(elem);
    }

    format!("[ {} ]", elems.join(" "))
}

/// Validates each segment of a single path (the pattern body, with any leading
/// `!` already stripped).
///
/// - Quoted segments whose inner content is `*`, `#`, or `!` are rejected.
/// - Unquoted segments that start with `!` are rejected (`!` is only valid as
///   a whole-pattern prefix, not inside a path).
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

/// Validates a single comma-separated pattern (after trimming).
fn validate_pattern(part: &str) -> Result<(), InputError> {
    if part.is_empty() {
        return Err(InputError::EvaluationWildcardEmpty);
    }
    if part.split_whitespace().count() > 1 {
        return Err(InputError::EvaluationWildcardInternalWhitespace);
    }

    // Strip the negation prefix to get the path body.
    let (is_exclusion, body) = match part.strip_prefix('!') {
        Some(b) => (true, b),
        None => (false, part),
    };

    // A bare `!` (nothing after the prefix) is invalid.
    if body.is_empty() {
        return Err(InputError::EvaluationWildcardBareSpecialChar);
    }

    if body.starts_with('.') {
        return Err(InputError::EvaluationWildcardStartsWithPeriod);
    }

    // Bare `#` as the entire body is invalid (internal sentinel character).
    // Bare `*` is valid — it means "evaluate everything".
    if body == "#" {
        return Err(InputError::EvaluationWildcardBareSpecialChar);
    }

    // Exclusion patterns must be exact paths — wildcards make no sense there.
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

    // ── valid patterns ───────────────────────────────────────────────────────

    #[test]
    fn star_in_path_valid() {
        let w: Wildcard = "packages.*.*".parse().unwrap();
        assert_eq!(w.patterns(), &["packages.*.*"]);
    }

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
    fn quoted_segment_python_style_valid() {
        let w: Wildcard = r#"packages.*."python3.12""#.parse().unwrap();
        assert_eq!(w.patterns(), &[r#"packages.*."python3.12""#]);
    }

    #[test]
    fn exclusion_pattern_valid() {
        let w: Wildcard = "packages.*.*,!packages.x86_64-linux.broken"
            .parse()
            .unwrap();
        assert_eq!(
            w.patterns(),
            &["packages.*.*", "!packages.x86_64-linux.broken"]
        );
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

    // ── get_eval_str ─────────────────────────────────────────────────────────

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

    // ── bare special chars ───────────────────────────────────────────────────

    #[test]
    fn bare_star_valid() {
        // `*` alone means "evaluate everything"
        let w: Wildcard = "*".parse().unwrap();
        assert_eq!(w.patterns(), &["*"]);
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
        // `!` is only valid as a whole-pattern prefix, not inside a segment
        assert!("my.!ignored".parse::<Wildcard>().is_err());
        assert!("my.!*".parse::<Wildcard>().is_err());
    }

    // ── quoted special chars ─────────────────────────────────────────────────

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

    // ── other invalid patterns ───────────────────────────────────────────────

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
