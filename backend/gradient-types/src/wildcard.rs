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
    Any,
    Deep,
    Glob(String),
    Lit(String),
}

pub fn segment_covers(pattern_segment: &str, name: &str) -> bool {
    covers(&segment(pattern_segment), name)
}

fn segment(pattern_segment: &str) -> Seg {
    match pattern_segment {
        "*" => Seg::Any,
        "**" => Seg::Deep,
        seg if seg.contains('*') => Seg::Glob(seg.to_string()),
        seg => Seg::Lit(seg.to_string()),
    }
}

fn covers(seg: &Seg, name: &str) -> bool {
    match seg {
        Seg::Any | Seg::Deep => true,
        Seg::Glob(glob) => glob_covers(glob, name),
        Seg::Lit(lit) => lit == name,
    }
}

fn glob_covers(glob: &str, name: &str) -> bool {
    let (pattern, subject) = (glob.as_bytes(), name.as_bytes());
    let (mut p, mut s) = (0, 0);
    let mut backtrack: Option<(usize, usize)> = None;

    while s < subject.len() {
        if p < pattern.len() && pattern[p] == subject[s] {
            p += 1;
            s += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            backtrack = Some((p, s));
            p += 1;
        } else if let Some((star, at)) = backtrack {
            p = star + 1;
            s = at + 1;
            backtrack = Some((star, at + 1));
        } else {
            return false;
        }
    }

    pattern[p..].iter().all(|b| *b == b'*')
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
            segment(&seg)
        };

        if matches!(next, Seg::Deep) && matches!(out.last(), Some(Seg::Deep)) {
            continue;
        }

        out.push(next);
    }

    out
}

fn pattern_covers(pattern: &str, path: &[String]) -> bool {
    let segs = pattern_segments(pattern);
    let Some(last) = segs.last() else {
        return false;
    };

    let depth = if matches!(last, Seg::Deep) {
        path.len() == segs.len() || path.len() == segs.len() + 1
    } else {
        path.len() == segs.len()
    };

    depth && segs.iter().zip(path).all(|(seg, name)| covers(seg, name))
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

fn validate_segments(path: &str) -> Result<(), InputError> {
    for (seg, is_quoted) in split_segments(path) {
        if is_quoted {
            let inner = seg.trim_matches('"');
            if matches!(inner, "*" | "**" | "#" | "!") {
                return Err(InputError::EvaluationWildcardBareSpecialChar);
            }
        } else if seg.starts_with('!') {
            return Err(InputError::EvaluationWildcardBareSpecialChar);
        } else if seg.contains('#') {
            return Err(InputError::EvaluationWildcardHashRemoved);
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

    if is_exclusion {
        for (seg, is_quoted) in split_segments(body) {
            if !is_quoted && seg.contains('*') {
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
    fn patterns_round_trip_through_to_string() {
        let cases = [
            ("packages.**,checks.**", "packages.**,checks.**"),
            ("packages.**, checks.**", "packages.**,checks.**"),
            (r#"my."wild.card".is.*"#, r#"my."wild.card".is.*"#),
            (
                r#"packages.x86_64-*.*,!packages.x86_64-linux.broken,my."wild.card".**"#,
                r#"packages.x86_64-*.*,!packages.x86_64-linux.broken,my."wild.card".**"#,
            ),
        ];
        for (input, expected) in cases {
            let w: Wildcard = input.parse().unwrap();
            assert_eq!(w.to_string(), expected, "{input}");
        }
        assert_eq!(
            "packages.**, checks.**"
                .parse::<Wildcard>()
                .unwrap()
                .patterns(),
            &["packages.**", "checks.**"]
        );
        assert_eq!(
            r#"packages.**,!packages.x86_64-linux."broken.pkg""#
                .parse::<Wildcard>()
                .unwrap()
                .patterns()
                .len(),
            2
        );
    }

    #[test]
    fn parse_rejects_malformed_patterns() {
        let cases = [
            "my.*,!my.ignored.*",
            "packages.**,!packages.x86_64-linux.*",
            "packages.x86_64-linux.#",
            "packages.x86-*.#",
            "#",
            "!",
            "my.!ignored",
            "my.!*",
            r#"my."*".not.allowed.*"#,
            r##"my."#".something"##,
            r#"my."!".something"#,
            r#"packages.**,!my."*".foo"#,
            "",
        ];
        for input in cases {
            assert!(input.parse::<Wildcard>().is_err(), "{input}");
        }
    }

    #[test]
    fn parse_reports_the_failing_rule() {
        let cases = [
            (
                "packages.**,,checks.**",
                InputError::EvaluationWildcardConsecutiveCommas,
            ),
            (" packages.**", InputError::EvaluationWildcardWhitespace),
            ("packages.** ", InputError::EvaluationWildcardWhitespace),
            (
                "packages .**",
                InputError::EvaluationWildcardInternalWhitespace,
            ),
            (
                ".packages.**",
                InputError::EvaluationWildcardStartsWithPeriod,
            ),
            (
                "packages.**,!",
                InputError::EvaluationWildcardBareSpecialChar,
            ),
            (
                "packages.**,!.packages",
                InputError::EvaluationWildcardStartsWithPeriod,
            ),
            (
                "packages.x86_64-linux.#",
                InputError::EvaluationWildcardHashRemoved,
            ),
            (
                "packages.x86_64-linux.*#",
                InputError::EvaluationWildcardHashRemoved,
            ),
            (
                "packages.**,!packages.aarch64-*",
                InputError::EvaluationWildcardExclusionWildcard,
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(input.parse::<Wildcard>().unwrap_err(), expected, "{input}");
        }
    }
}

#[cfg(test)]
mod matches_tests {
    use super::*;

    fn m(pattern: &str, attr: &str) -> bool {
        pattern.parse::<Wildcard>().unwrap().matches(attr)
    }

    #[test]
    fn single_pattern_matching_follows_the_segment_rules() {
        let cases = [
            (
                "packages.x86_64-linux.hello",
                "packages.x86_64-linux.hello",
                true,
            ),
            (
                "packages.x86_64-linux.hello",
                "packages.x86_64-linux.world",
                false,
            ),
            (
                "packages.x86_64-linux.hello",
                "packages.x86_64-linux",
                false,
            ),
            ("my.*.test", "my.foo.test", true),
            ("my.*.test", "my.test", false),
            ("my.*.test", "my.foo.bar.test", false),
            (
                "packages.x86_64-linux.*",
                "packages.x86_64-linux.hello",
                true,
            ),
            (
                "packages.x86_64-linux.*",
                "packages.x86_64-linux.py.hello",
                false,
            ),
            ("packages.x86_64-linux.*", "packages.x86_64-linux", false),
            ("packages.*", "packages.x86_64-linux", true),
            ("packages.*", "packages", false),
            ("packages.*.*", "packages.x86_64-linux.hello", true),
            ("packages.*.*", "packages.x86_64-linux", false),
            ("packages.**", "packages.x86_64-linux", true),
            ("packages.**", "packages.x86_64-linux.hello", true),
            ("packages.**", "packages.x86_64-linux.py.hello", false),
            ("**.**", "packages.x86_64-linux", true),
            ("**", "hello", true),
            ("**", "packages.hello", true),
            ("**", "packages.x86_64-linux.hello", false),
            ("*", "hello", true),
            ("*", "packages.hello", false),
            ("*", "", false),
            ("packages.*", "", false),
        ];
        for (pattern, attr, expected) in cases {
            assert_eq!(m(pattern, attr), expected, "{pattern} vs {attr}");
        }
    }

    #[test]
    fn a_glob_segment_matches_within_its_own_level() {
        let cases = [
            (
                "packages.x86_64-*.hello",
                "packages.x86_64-linux.hello",
                true,
            ),
            (
                "packages.x86_64-*.hello",
                "packages.aarch64-linux.hello",
                false,
            ),
            (
                "packages.*-linux.hello",
                "packages.aarch64-linux.hello",
                true,
            ),
            ("packages.-*-.hello", "packages.-linux-.hello", true),
            ("packages.-*-.hello", "packages.-linux.hello", false),
            ("packages.x86*-*", "packages.x86_64-linux", true),
            (
                "packages.*.*-linux.*",
                "packages.foo.x86_64-linux.hello",
                true,
            ),
            (
                "packages.*.*-linux.*",
                "packages.foo.x86_64-linux.py.hello",
                false,
            ),
        ];
        for (pattern, attr, expected) in cases {
            assert_eq!(m(pattern, attr), expected, "{pattern} vs {attr}");
        }
        assert!(m("packages.**", "packages.x86_64-linux.hello"));
        assert!(!m("packages.*", "packages.x86_64-linux.hello"));
    }

    #[test]
    fn include_and_exclude_patterns_combine() {
        let any: Wildcard = "packages.**,checks.**".parse().unwrap();
        assert!(any.matches("packages.x86_64-linux.hello"));
        assert!(any.matches("checks.x86_64-linux.fmt"));
        assert!(!any.matches("devShells.x86_64-linux.default"));

        let excluding: Wildcard = "packages.**,!packages.x86_64-linux.broken".parse().unwrap();
        assert!(excluding.matches("packages.x86_64-linux.hello"));
        assert!(!excluding.matches("packages.x86_64-linux.broken"));
        assert!(excluding.matches("packages.aarch64-linux.broken"));
    }

    #[test]
    fn quoted_segments_compare_by_inner_content() {
        let cases = [
            (
                r#"packages.*."python3.12""#,
                r#"packages.x86_64-linux."python3.12""#,
                true,
            ),
            (
                r#"packages.*."python3.12""#,
                "packages.x86_64-linux.python3.12",
                false,
            ),
            (r#"my."wild.card".test"#, r#"my."wild.card".test"#, true),
            (r#"my."wild.card".test"#, "my.wild.card.test", false),
            ("packages.**", r#"packages."x86_64-linux".hello"#, true),
        ];
        for (pattern, attr, expected) in cases {
            assert_eq!(m(pattern, attr), expected, "{pattern} vs {attr}");
        }
    }
}
